//! Logic and data types relating to tallying validators' votes for pieces of
//! data stored in the ledger, where those pieces of data should only be acted
//! on once they have received enough votes
use std::collections::{BTreeMap, BTreeSet, HashMap};

use borsh::{BorshDeserialize, BorshSchema, BorshSerialize};
use eyre::{eyre, Result};

use super::ChangedKeys;
use crate::ledger::eth_bridge::storage::vote_tallies;
use crate::ledger::protocol::transactions::read;
use crate::ledger::storage::traits::StorageHasher;
use crate::ledger::storage::{DBIter, Storage, DB};
use crate::types::address::Address;
use crate::types::storage::BlockHeight;
use crate::types::voting_power::FractionalVotingPower;

pub(super) mod storage;

/// The addresses of validators that voted for something, and the block
/// heights at which they voted. We use a [`BTreeMap`] to enforce that a
/// validator (as uniquely identified by an [`Address`]) may vote at most once,
/// and their vote must be associated with a specific [`BlockHeight`]. Their
/// voting power at that block height is what is used when calculating whether
/// something has enough voting power behind it or not.
pub type Votes = BTreeMap<Address, BlockHeight>;

#[derive(
    Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, BorshSchema,
)]
/// Represents all the information needed to tally a piece of data that may be
/// voted for over multiple epochs
pub struct Tally {
    /// The total voting power that's voted for this event across all epochs
    pub voting_power: FractionalVotingPower,
    /// The votes which have been counted towards `voting_power`. Note that
    /// validators may submit multiple votes at different block heights for
    /// the same thing, but ultimately only one vote per validator will be
    /// used when tallying voting power.
    pub seen_by: Votes,
    /// Whether this event has been acted on or not - this should only ever
    /// transition from `false` to `true`, once there is enough voting power
    pub seen: bool,
}

/// Calculate a new [`Tally`] based on some validators' fractional voting powers
/// as specific block heights
pub fn calculate_new(
    seen_by: Votes,
    voting_powers: &HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<Tally> {
    let mut seen_by_voting_power = FractionalVotingPower::default();
    for (validator, block_height) in seen_by.iter() {
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
    Ok(Tally {
        voting_power: seen_by_voting_power,
        seen_by,
        seen: newly_confirmed,
    })
}

/// Calculate an updated [`Tally`] based on one that is in storage under `keys`,
/// with some new `voters`.
pub fn calculate_updated<D, H, T>(
    store: &mut Storage<D, H>,
    keys: &vote_tallies::Keys<T>,
    _voters: &HashMap<Address, FractionalVotingPower>,
) -> Result<(Tally, ChangedKeys)>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
    T: BorshDeserialize,
{
    // TODO(namada#515): implement this
    let _body: T = read::value(store, &keys.body())?;
    let seen: bool = read::value(store, &keys.seen())?;
    let seen_by: Votes = read::value(store, &keys.seen_by())?;
    let voting_power: FractionalVotingPower =
        read::value(store, &keys.voting_power())?;

    let tally = Tally {
        voting_power,
        seen_by,
        seen,
    };

    tracing::warn!(
        ?tally,
        "Updating events is not implemented yet, so the returned vote tally \
         will be identical to the one in storage",
    );
    Ok((tally, ChangedKeys::default()))
}

/// Deterministically constructs a [`Votes`] map from a set of validator
/// addresses and the block heights they signed something at. We arbitrarily
/// take the earliest block height for each validator address encountered.
pub fn dedupe(signers: BTreeSet<(Address, BlockHeight)>) -> Votes {
    signers.into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::types::address;
    use crate::types::storage::BlockHeight;

    #[test]
    fn test_dedupe_empty() {
        let signers = BTreeSet::new();

        let deduped = dedupe(signers);

        assert_eq!(deduped, Votes::new());
    }

    #[test]
    fn test_dedupe_single_vote() {
        let sole_validator = address::testing::established_address_1();
        let votes = [(sole_validator, BlockHeight(100))];
        let signers = BTreeSet::from(votes.clone());

        let deduped = dedupe(signers);

        assert_eq!(deduped, Votes::from(votes));
    }

    #[test]
    fn test_dedupe_multiple_votes_same_voter() {
        let sole_validator = address::testing::established_address_1();
        let earliest_vote_height = 100;
        let earliest_vote =
            (sole_validator.clone(), BlockHeight(earliest_vote_height));
        let votes = [
            earliest_vote.clone(),
            (
                sole_validator.clone(),
                BlockHeight(earliest_vote_height + 1),
            ),
            (sole_validator, BlockHeight(earliest_vote_height + 100)),
        ];
        let signers = BTreeSet::from(votes);

        let deduped = dedupe(signers);

        assert_eq!(deduped, Votes::from([earliest_vote]));
    }

    #[test]
    fn test_dedupe_multiple_votes_multiple_voters() {
        let validator_1 = address::testing::established_address_1();
        let validator_2 = address::testing::established_address_2();
        let validator_1_earliest_vote_height = 100;
        let validator_1_earliest_vote = (
            validator_1.clone(),
            BlockHeight(validator_1_earliest_vote_height),
        );
        let validator_2_earliest_vote_height = 200;
        let validator_2_earliest_vote = (
            validator_2.clone(),
            BlockHeight(validator_2_earliest_vote_height),
        );
        let votes = [
            validator_1_earliest_vote.clone(),
            (
                validator_1.clone(),
                BlockHeight(validator_1_earliest_vote_height + 1),
            ),
            (
                validator_1,
                BlockHeight(validator_1_earliest_vote_height + 100),
            ),
            validator_2_earliest_vote.clone(),
            (
                validator_2.clone(),
                BlockHeight(validator_2_earliest_vote_height + 1),
            ),
            (
                validator_2,
                BlockHeight(validator_2_earliest_vote_height + 100),
            ),
        ];
        let signers = BTreeSet::from(votes);

        let deduped = dedupe(signers);

        assert_eq!(
            deduped,
            Votes::from([validator_1_earliest_vote, validator_2_earliest_vote])
        );
    }
}
