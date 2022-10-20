//! Code for handling
//! [`namada::types::transaction::protocol::ProtocolTxType::EthereumEvents`]
//! transactions.
mod eth_msgs;
mod events;
mod read;
mod update;
mod utils;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use borsh::BorshSerialize;
use eth_msgs::{EthMsg, EthMsgUpdate};
use eyre::{eyre, Result};
use namada::ledger::eth_bridge::storage::eth_msgs::Keys;
use namada::ledger::pos::types::WeightedValidator;
use namada::ledger::storage::{DBIter, Storage, StorageHasher, DB};
use namada::types::address::Address;
use namada::types::ethereum_events::EthereumEvent;
use namada::types::storage::{self, BlockHeight};
use namada::types::transaction::TxResult;
use namada::types::vote_extensions::ethereum_events::MultiSignedEthEvent;
use namada::types::voting_power::FractionalVotingPower;

use crate::node::ledger::shell::queries::QueriesExt;

/// The keys changed while applying a protocol transaction
type ChangedKeys = BTreeSet<storage::Key>;

/// Applies derived state changes to storage, based on Ethereum `events` which
/// were newly seen by some active validator(s) in the last epoch. For `events`
/// which have been seen by enough voting power, extra state changes may take
/// place, such as minting of wrapped ERC20s.
///
/// This function is deterministic based on some existing blockchain state and
/// the passed `events`.
pub(crate) fn apply_derived_tx<D, H>(
    storage: &mut Storage<D, H>,
    events: Vec<MultiSignedEthEvent>, /* TODO: should this be a BTreeMap<EthereumEvent, BTreeSet<(Address, BlockHeight)>> */
) -> Result<TxResult>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    if events.is_empty() {
        return Ok(TxResult::default());
    }
    tracing::info!(
        ethereum_events = events.len(),
        "Applying state updates derived from Ethereum events found in \
         protocol transaction"
    );

    let events = dedupe(events);
    // from a type perspective we ultimately want something like a
    // `BTreeMap<EthereumEvent, BTreeMap<Address, BlockHeight>>` where each seen
    // event has an associated map of validators who saw it mapped to the
    // earliest block height at which they saw it

    let voting_powers = get_voting_powers(storage, &events)?;

    let updates = events.into_iter().map(Into::<EthMsgUpdate>::into).collect();

    let changed_keys = apply_updates(storage, updates, voting_powers)?;

    Ok(TxResult {
        changed_keys,
        ..Default::default()
    })
}

/// Dedupe consumes `events` and returns a version of it where the `signers`
/// associated with each event has at most one vote per validator, where this
/// vote is the earliest by block height among their included votes
fn dedupe(events: Vec<MultiSignedEthEvent>) -> Vec<MultiSignedEthEvent> {
    events
        .into_iter()
        .map(
            |MultiSignedEthEvent {
                 event,
                 signers: votes,
             }| {
                let event_hash = event.hash();
                let unique_voters: BTreeSet<Address> =
                    votes.iter().map(|(addr, _)| addr.to_owned()).collect();
                tracing::debug!(
                    event = ?event_hash,
                    votes = votes.len(),
                    unique_voters = unique_voters.len(),
                    "Found votes for event"
                );
                let mut earliest_votes = BTreeSet::default();
                for voter in unique_voters {
                    let earliest_vote_height = votes
                        .iter()
                        .filter(|(addr, _)| *addr == voter)
                        .map(|(_, height)| *height)
                        .min()
                        .expect("every voter must have at least one vote");
                    // TODO: remove the above expect!
                    _ = earliest_votes.insert((voter, earliest_vote_height));
                }
                MultiSignedEthEvent {
                    event,
                    signers: earliest_votes,
                }
            },
        )
        .collect()
}

fn get_active_validators<D, H>(
    storage: &Storage<D, H>,
    block_heights: HashSet<BlockHeight>,
) -> BTreeMap<BlockHeight, BTreeSet<WeightedValidator<Address>>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let mut active_validators = BTreeMap::default();
    for height in block_heights.into_iter() {
        let epoch = storage.get_epoch(height).expect(
            "The epoch of the last block height should always be known",
        );
        _ = active_validators
            .insert(height, storage.get_active_validators(Some(epoch)));
    }
    active_validators
}

/// Constructs a map of all validators who voted for an event to their
/// fractional voting power for block heights at which they voted for an event
fn get_voting_powers<D, H>(
    storage: &Storage<D, H>,
    events: &[MultiSignedEthEvent],
) -> Result<HashMap<(Address, BlockHeight), FractionalVotingPower>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let voters = utils::get_votes_for_events(events.iter());
    tracing::debug!(?voters, "Got validators who voted on at least one event");

    let active_validators = get_active_validators(
        storage,
        voters.iter().map(|(_, h)| h.to_owned()).collect(),
    );
    tracing::debug!(
        n = active_validators.len(),
        "got active validators - {:#?}",
        active_validators,
    );

    let voting_powers =
        utils::get_voting_powers_for_selected(&active_validators, voters)?;
    tracing::debug!(
        ?voting_powers,
        "got voting powers for relevant validators"
    );

    Ok(voting_powers)
}

/// Apply an Ethereum state update + act on any events which are confirmed
pub(super) fn apply_updates<D, H>(
    storage: &mut Storage<D, H>,
    updates: HashSet<EthMsgUpdate>,
    voting_powers: HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<ChangedKeys>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    tracing::debug!(
        updates.len = updates.len(),
        ?voting_powers,
        "Applying Ethereum state update transaction"
    );

    let mut changed_keys = BTreeSet::default();
    let mut confirmed = vec![];
    for update in updates {
        // The order in which updates are applied to storage does not matter.
        // The final storage state will be the same regardless.
        let (mut changed, newly_confirmed) =
            apply_update(storage, update.clone(), &voting_powers)?;
        changed_keys.append(&mut changed);
        if newly_confirmed {
            confirmed.push(update.body);
        }
    }
    if confirmed.is_empty() {
        tracing::debug!("No events were newly confirmed");
        return Ok(changed_keys);
    }
    tracing::debug!(n = confirmed.len(), "Events were newly confirmed",);

    // Right now, the order in which events are acted on does not matter.
    // For `TransfersToNamada` events, they can happen in any order.
    for event in &confirmed {
        let mut changed = events::act_on(storage, event)?;
        changed_keys.append(&mut changed);
    }
    Ok(changed_keys)
}

/// Apply an [`EthMsgUpdate`] to storage. Returns any keys changed and whether
/// the event was newly seen.
fn apply_update<D, H>(
    storage: &mut Storage<D, H>,
    update: EthMsgUpdate,
    voting_powers: &HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<(ChangedKeys, bool)>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let eth_msg_keys = Keys::from(&update.body);

    // we arbitrarily look at whether the seen key is present to
    // determine if the /eth_msg already exists in storage, but maybe there
    // is a less arbitrary way to do this
    let (exists_in_storage, _) = storage.has_key(&eth_msg_keys.seen())?;

    let (eth_msg_post, changed, confirmed) = if !exists_in_storage {
        let (eth_msg_post, changed) =
            calculate_new_eth_msg(update, voting_powers)?;
        let confirmed = eth_msg_post.seen;
        (eth_msg_post, changed, confirmed)
    } else {
        let (eth_msg_post, changed) =
            calculate_updated_eth_msg(storage, update, voting_powers)?;
        let confirmed =
            eth_msg_post.seen && changed.contains(&eth_msg_keys.seen());
        (eth_msg_post, changed, confirmed)
    };
    write_eth_msg(storage, &eth_msg_keys, &eth_msg_post)?;
    Ok((changed, confirmed))
}

fn calculate_new_eth_msg(
    update: EthMsgUpdate,
    voting_powers: &HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<(EthMsg, ChangedKeys)> {
    let eth_msg_keys = Keys::from(&update.body);
    tracing::debug!(%eth_msg_keys.prefix, "Ethereum event not seen before by any validator");

    let mut seen_by_voting_power = FractionalVotingPower::default();
    for (validator, block_height) in &update.seen_by {
        match voting_powers
            .get(&(validator.to_owned(), block_height.to_owned()))
        {
            Some(voting_power) => seen_by_voting_power += voting_power,
            None => {
                return Err(eyre!(
                    "voting power was not provided for validator {}",
                    validator
                ));
            }
        };
    }

    let newly_confirmed =
        seen_by_voting_power > FractionalVotingPower::TWO_THIRDS;
    Ok((
        EthMsg {
            body: update.body,
            voting_power: seen_by_voting_power,
            seen_by: update
                .seen_by
                .into_iter()
                .map(|(validator, _)| validator)
                .collect(),
            seen: newly_confirmed,
        },
        eth_msg_keys.into_iter().collect(),
    ))
}

fn calculate_updated_eth_msg<D, H>(
    store: &mut Storage<D, H>,
    update: EthMsgUpdate,
    voting_powers: &HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<(EthMsg, ChangedKeys)>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let eth_msg_keys = Keys::from(&update.body);
    tracing::debug!(
        %eth_msg_keys.prefix,
        "Ethereum event already exists in storage",
    );
    let body: EthereumEvent = read::value(store, &eth_msg_keys.body())?;
    let seen: bool = read::value(store, &eth_msg_keys.seen())?;
    let seen_by: BTreeSet<Address> =
        read::value(store, &eth_msg_keys.seen_by())?;
    let voting_power: FractionalVotingPower =
        read::value(store, &eth_msg_keys.voting_power())?;

    let eth_msg_pre = EthMsg {
        body,
        voting_power,
        seen_by,
        seen,
    };
    tracing::debug!("Read EthMsg - {:#?}", &eth_msg_pre);
    Ok(calculate_diff(eth_msg_pre, update, voting_powers))
}

fn calculate_diff(
    eth_msg: EthMsg,
    _update: EthMsgUpdate,
    _voting_powers: &HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> (EthMsg, ChangedKeys) {
    tracing::warn!(
        "Updating Ethereum events is not yet implemented, so this Ethereum \
         event won't change"
    );
    (eth_msg, BTreeSet::default())
}

fn write_eth_msg<D, H>(
    storage: &mut Storage<D, H>,
    eth_msg_keys: &Keys,
    eth_msg: &EthMsg,
) -> Result<()>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    tracing::debug!("writing EthMsg - {:#?}", eth_msg);
    storage.write(&eth_msg_keys.body(), &eth_msg.body.try_to_vec()?)?;
    storage.write(&eth_msg_keys.seen(), &eth_msg.seen.try_to_vec()?)?;
    storage.write(&eth_msg_keys.seen_by(), &eth_msg.seen_by.try_to_vec()?)?;
    storage.write(
        &eth_msg_keys.voting_power(),
        &eth_msg.voting_power.try_to_vec()?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap, HashSet};

    use borsh::BorshDeserialize;
    use namada::ledger::eth_bridge::storage::wrapped_erc20s;
    use namada::ledger::pos::namada_proof_of_stake::epoched::Epoched;
    use namada::ledger::pos::namada_proof_of_stake::PosBase;
    use namada::ledger::pos::types::{ValidatorSet, VotingPower};
    use namada::ledger::storage::mockdb::MockDB;
    use namada::ledger::storage::testing::TestStorage;
    use namada::ledger::storage::Sha256Hasher;
    use namada::types::address;
    use namada::types::ethereum_events::testing::{
        arbitrary_amount, arbitrary_eth_address, arbitrary_nonce,
        DAI_ERC20_ETH_ADDRESS,
    };
    use namada::types::ethereum_events::{EthereumEvent, TransferToNamada};
    use namada::types::token::Amount;
    use storage::BlockHeight;

    use super::*;

    #[test]
    fn test_get_voting_powers_empty() {
        let storage = TestStorage::default();

        let result = get_voting_powers(&storage, &[]);

        assert!(result.unwrap().is_empty())
    }

    #[test]
    fn test_get_voting_powers_one_validator() {
        let voter = address::testing::established_address_1();
        let voting_height = BlockHeight(100);
        let storage = set_up_test_storage(HashMap::from_iter(vec![(
            voter.clone(),
            VotingPower::from(100),
        )]));

        let event = EthereumEvent::TransfersToNamada {
            nonce: 1.into(),
            transfers: vec![TransferToNamada {
                amount: Amount::from(100),
                asset: DAI_ERC20_ETH_ADDRESS,
                receiver: address::testing::established_address_1(),
            }],
        };
        let signers = BTreeSet::from_iter(vec![(voter.clone(), voting_height)]);

        let events = vec![MultiSignedEthEvent { event, signers }];

        let result = get_voting_powers(&storage, &events);

        assert_eq!(
            result.unwrap(),
            HashMap::from_iter(vec![(
                (voter, voting_height),
                FractionalVotingPower::new(1, 1).unwrap()
            )])
        );
    }

    #[test]
    /// Assert that `dedupe` returns events with only the earliest votes for
    /// that event by any given validator
    fn test_dedupe() {
        let event = EthereumEvent::TransfersToNamada {
            nonce: 1.into(),
            transfers: vec![TransferToNamada {
                amount: Amount::from(100),
                asset: DAI_ERC20_ETH_ADDRESS,
                receiver: address::testing::established_address_1(),
            }],
        };
        let signers = BTreeSet::from_iter(vec![
            (address::testing::established_address_1(), BlockHeight(100)),
            (address::testing::established_address_2(), BlockHeight(91)),
            (address::testing::established_address_1(), BlockHeight(102)),
            (address::testing::established_address_2(), BlockHeight(90)),
            (address::testing::established_address_1(), BlockHeight(101)),
        ]);

        let events = vec![MultiSignedEthEvent {
            event: event.clone(),
            signers,
        }];

        let result = dedupe(events);

        assert_eq!(
            result,
            vec![MultiSignedEthEvent {
                event,
                signers: BTreeSet::from([
                    (
                        address::testing::established_address_1(),
                        BlockHeight(100)
                    ),
                    (
                        address::testing::established_address_2(),
                        BlockHeight(90)
                    )
                ])
            }],
        );
    }

    #[test]
    /// Test applying a `TransfersToNamada` batch containing a single transfer
    fn test_apply_single_transfer() -> Result<()> {
        let sole_validator = address::testing::gen_established_address();
        let receiver = address::testing::established_address_2();

        let amount = arbitrary_amount();
        let asset = arbitrary_eth_address();
        let body = EthereumEvent::TransfersToNamada {
            nonce: arbitrary_nonce(),
            transfers: vec![TransferToNamada {
                amount,
                asset: asset.clone(),
                receiver: receiver.clone(),
            }],
        };
        let update = EthMsgUpdate {
            body: body.clone(),
            seen_by: BTreeSet::from_iter(vec![(
                sole_validator.clone(),
                BlockHeight(100),
            )]),
        };
        let updates = HashSet::from_iter(vec![update]);
        let voting_powers = HashMap::from_iter(vec![(
            (sole_validator.clone(), BlockHeight(100)),
            FractionalVotingPower::new(1, 1).unwrap(),
        )]);
        let mut storage = TestStorage::default();

        let changed_keys = apply_updates(&mut storage, updates, voting_powers)?;

        let eth_msg_keys: Keys = (&body).into();
        let wrapped_erc20_keys: wrapped_erc20s::Keys = (&asset).into();
        assert_eq!(
            BTreeSet::from_iter(vec![
                eth_msg_keys.body(),
                eth_msg_keys.seen(),
                eth_msg_keys.seen_by(),
                eth_msg_keys.voting_power(),
                wrapped_erc20_keys.balance(&receiver),
                wrapped_erc20_keys.supply(),
            ]),
            changed_keys
        );

        let (body_bytes, _) = storage.read(&eth_msg_keys.body())?;
        let body_bytes = body_bytes.unwrap();
        assert_eq!(EthereumEvent::try_from_slice(&body_bytes)?, body);

        let (seen_bytes, _) = storage.read(&eth_msg_keys.seen())?;
        let seen_bytes = seen_bytes.unwrap();
        assert!(bool::try_from_slice(&seen_bytes)?);

        let (seen_by_bytes, _) = storage.read(&eth_msg_keys.seen_by())?;
        let seen_by_bytes = seen_by_bytes.unwrap();
        assert_eq!(
            Vec::<Address>::try_from_slice(&seen_by_bytes)?,
            vec![sole_validator]
        );

        let (voting_power_bytes, _) =
            storage.read(&eth_msg_keys.voting_power())?;
        let voting_power_bytes = voting_power_bytes.unwrap();
        assert_eq!(<(u64, u64)>::try_from_slice(&voting_power_bytes)?, (1, 1));

        let (wrapped_erc20_balance_bytes, _) =
            storage.read(&wrapped_erc20_keys.balance(&receiver))?;
        let wrapped_erc20_balance_bytes = wrapped_erc20_balance_bytes.unwrap();
        assert_eq!(
            Amount::try_from_slice(&wrapped_erc20_balance_bytes)?,
            amount
        );

        let (wrapped_erc20_supply_bytes, _) =
            storage.read(&wrapped_erc20_keys.supply())?;
        let wrapped_erc20_supply_bytes = wrapped_erc20_supply_bytes.unwrap();
        assert_eq!(
            Amount::try_from_slice(&wrapped_erc20_supply_bytes)?,
            amount
        );

        Ok(())
    }

    /// Set up a `TestStorage` initialized at genesis
    fn set_up_test_storage(
        active_validators: HashMap<Address, VotingPower>,
    ) -> Storage<MockDB, Sha256Hasher> {
        let mut storage = TestStorage::default();
        let validator_set = ValidatorSet {
            active: active_validators
                .into_iter()
                .map(|(address, voting_power)| WeightedValidator {
                    voting_power,
                    address,
                })
                .collect(),
            inactive: BTreeSet::default(),
        };
        let validator_sets = Epoched::init_at_genesis(validator_set, 1);
        storage.write_validator_set(&validator_sets);
        storage
    }

    #[test]
    /// Test applying a single transfer via `apply_derived_tx`, where an event
    /// has enough voting power behind it for it to be applied at the same time
    /// that it is recorded in storage
    fn test_apply_derived_tx_new_event_mint_immediately() {
        let sole_validator = address::testing::established_address_2();
        let mut storage = set_up_test_storage(HashMap::from_iter(vec![(
            sole_validator.clone(),
            VotingPower::from(100),
        )]));
        let receiver = address::testing::established_address_1();

        let event = EthereumEvent::TransfersToNamada {
            nonce: 1.into(),
            transfers: vec![TransferToNamada {
                amount: Amount::from(100),
                asset: DAI_ERC20_ETH_ADDRESS,
                receiver: receiver.clone(),
            }],
        };

        let result = apply_derived_tx(
            &mut storage,
            vec![MultiSignedEthEvent {
                event: event.clone(),
                signers: BTreeSet::from([(sole_validator, BlockHeight(100))]),
            }],
        );

        let tx_result = match result {
            Ok(tx_result) => tx_result,
            Err(err) => panic!("unexpected error: {:#?}", err),
        };

        assert_eq!(
            tx_result.gas_used, 0,
            "No gas should be used for a derived transaction"
        );
        let eth_msg_keys = Keys::from(&event);
        let dai_keys = wrapped_erc20s::Keys::from(&DAI_ERC20_ETH_ADDRESS);
        assert_eq!(
            tx_result.changed_keys,
            BTreeSet::from_iter(vec![
                eth_msg_keys.body(),
                eth_msg_keys.seen(),
                eth_msg_keys.seen_by(),
                eth_msg_keys.voting_power(),
                dai_keys.balance(&receiver),
                dai_keys.supply(),
            ])
        );
        assert!(tx_result.vps_result.accepted_vps.is_empty());
        assert!(tx_result.vps_result.rejected_vps.is_empty());
        assert!(tx_result.vps_result.errors.is_empty());
        assert!(tx_result.initialized_accounts.is_empty());
        assert!(tx_result.ibc_event.is_none());
    }

    /// Test calling apply_derived_tx for an event that isn't backed by enough
    /// voting power to be acted on immediately
    #[test]
    fn test_apply_derived_tx_new_event_dont_mint() {
        let validator_a = address::testing::established_address_2();
        let validator_b = address::testing::established_address_3();
        let mut storage = set_up_test_storage(HashMap::from_iter(vec![
            (validator_a.clone(), VotingPower::from(100)),
            (validator_b, VotingPower::from(100)),
        ]));
        let receiver = address::testing::established_address_1();

        let event = EthereumEvent::TransfersToNamada {
            nonce: 1.into(),
            transfers: vec![TransferToNamada {
                amount: Amount::from(100),
                asset: DAI_ERC20_ETH_ADDRESS,
                receiver,
            }],
        };

        let result = apply_derived_tx(
            &mut storage,
            vec![MultiSignedEthEvent {
                event: event.clone(),
                signers: BTreeSet::from([(validator_a, BlockHeight(100))]),
            }],
        );
        let tx_result = match result {
            Ok(tx_result) => tx_result,
            Err(err) => panic!("unexpected error: {:#?}", err),
        };

        let eth_msg_keys = Keys::from(&event);
        assert_eq!(
            tx_result.changed_keys,
            BTreeSet::from_iter(vec![
                eth_msg_keys.body(),
                eth_msg_keys.seen(),
                eth_msg_keys.seen_by(),
                eth_msg_keys.voting_power(),
            ]),
            "The Ethereum event should have been recorded, but no minting \
             should have happened yet as it has only been seen by 1/2 the \
             voting power so far"
        );
    }
}
