//! Proof-of-Stake native validity predicate.

use std::collections::BTreeSet;
use std::panic::{RefUnwindSafe, UnwindSafe};

use borsh::BorshDeserialize;
use itertools::Itertools;
pub use namada_proof_of_stake;
pub use namada_proof_of_stake::parameters::PosParams;
pub use namada_proof_of_stake::types::{
    self, Slash, Slashes, TotalVotingPowers, ValidatorStates,
    ValidatorVotingPowers,
};
use namada_proof_of_stake::validation::validate;
use namada_proof_of_stake::{validation, PosReadOnly};
use thiserror::Error;

use super::{
    bond_key, is_bond_key, is_params_key, is_total_voting_power_key,
    is_unbond_key, is_validator_set_key,
    is_validator_staking_reward_address_key, is_validator_total_deltas_key,
    is_validator_voting_power_key, params_key, staking_token_address,
    total_voting_power_key, unbond_key, validator_consensus_key_key,
    validator_set_key, validator_slashes_key,
    validator_staking_reward_address_key, validator_state_key,
    validator_total_deltas_key, validator_voting_power_key, BondId, Bonds,
    Unbonds, ValidatorConsensusKeys, ValidatorSets, ValidatorTotalDeltas,
};
use crate::impl_pos_read_only;
use crate::ledger::governance::vp::is_proposal_accepted;
use crate::ledger::native_vp::{
    self, Ctx, CtxPostStorageRead, CtxPreStorageRead, NativeVp,
};
use crate::ledger::pos::{
    is_validator_address_raw_hash_key, is_validator_consensus_key_key,
    is_validator_state_key,
};
use crate::ledger::storage::{self as ledger_storage, StorageHasher};
use crate::ledger::storage_api::{self, StorageRead};
use crate::types::address::{Address, InternalAddress};
use crate::types::storage::{Key, KeySeg};
use crate::types::token;
use crate::vm::WasmCacheAccess;

#[allow(missing_docs)]
#[derive(Error, Debug)]
pub enum Error {
    #[error("Native VP error: {0}")]
    NativeVpError(native_vp::Error),
    #[error("Storage error: {0}")]
    StorageApi(storage_api::Error),
}

/// PoS functions result
pub type Result<T> = std::result::Result<T, Error>;

/// Proof-of-Stake validity predicate
pub struct PosVP<'a, DB, H, CA>
where
    DB: ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter>,
    H: StorageHasher,
    CA: WasmCacheAccess,
{
    /// Context to interact with the host structures.
    pub ctx: Ctx<'a, DB, H, CA>,
}

impl<'a, DB, H, CA> PosVP<'a, DB, H, CA>
where
    DB: 'static + ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter>,
    H: 'static + StorageHasher,
    CA: 'static + WasmCacheAccess,
{
    /// Instantiate a `PosVP`.
    pub fn new(ctx: Ctx<'a, DB, H, CA>) -> Self {
        Self { ctx }
    }
}

// TODO this is temporarily to run PoS native VP in a new thread to avoid
// crashing the ledger (in apps/src/lib/node/ledger/protocol/mod.rs). The
// RefCells contained within PosVP are not thread-safe, but each thread has its
// own instances.
impl<DB, H, CA> UnwindSafe for PosVP<'_, DB, H, CA>
where
    DB: 'static + ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter>,
    H: 'static + StorageHasher,
    CA: 'static + WasmCacheAccess,
{
}

// TODO this is temporarily to run PoS native VP in a new thread to avoid
// crashing the ledger (in apps/src/lib/node/ledger/protocol/mod.rs). The
// RefCells contained within PosVP are not thread-safe, but each thread has its
// own instances.
impl<DB, H, CA> RefUnwindSafe for PosVP<'_, DB, H, CA>
where
    DB: 'static + ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter>,
    H: 'static + StorageHasher,
    CA: 'static + WasmCacheAccess,
{
}

impl<'a, DB, H, CA> NativeVp for PosVP<'a, DB, H, CA>
where
    DB: 'static + ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter>,
    H: 'static + StorageHasher,
    CA: 'static + WasmCacheAccess,
{
    type Error = Error;

    const ADDR: InternalAddress = InternalAddress::PoS;

    fn validate_tx(
        &self,
        tx_data: &[u8],
        keys_changed: &BTreeSet<Key>,
        _verifiers: &BTreeSet<Address>,
    ) -> Result<bool> {
        use validation::Data;
        use validation::DataUpdate::{self, *};
        use validation::ValidatorUpdate::*;

        let addr = Address::Internal(Self::ADDR);
        let mut changes: Vec<DataUpdate<_, _, _, _>> = vec![];
        let current_epoch = self.ctx.pre().get_block_epoch()?;

        for key in keys_changed {
            if is_params_key(key) {
                let proposal_id = u64::try_from_slice(tx_data).ok();
                match proposal_id {
                    Some(id) => return Ok(is_proposal_accepted(&self.ctx, id)),
                    _ => return Ok(false),
                }
            } else if is_validator_set_key(key) {
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    ValidatorSets::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    ValidatorSets::try_from_slice(&bytes[..]).ok()
                });
                changes.push(ValidatorSet(Data { pre, post }));
            } else if let Some(validator) = is_validator_state_key(key) {
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    ValidatorStates::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    ValidatorStates::try_from_slice(&bytes[..]).ok()
                });
                changes.push(Validator {
                    address: validator.clone(),
                    update: State(Data { pre, post }),
                });
            } else if let Some(validator) =
                is_validator_staking_reward_address_key(key)
            {
                let pre =
                    self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                        Address::try_from_slice(&bytes[..]).ok()
                    });
                let post =
                    self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                        Address::try_from_slice(&bytes[..]).ok()
                    });
                changes.push(Validator {
                    address: validator.clone(),
                    update: StakingRewardAddress(Data { pre, post }),
                });
            } else if let Some(validator) = is_validator_consensus_key_key(key)
            {
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    ValidatorConsensusKeys::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    ValidatorConsensusKeys::try_from_slice(&bytes[..]).ok()
                });
                changes.push(Validator {
                    address: validator.clone(),
                    update: ConsensusKey(Data { pre, post }),
                });
            } else if let Some(validator) = is_validator_total_deltas_key(key) {
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    ValidatorTotalDeltas::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    ValidatorTotalDeltas::try_from_slice(&bytes[..]).ok()
                });
                changes.push(Validator {
                    address: validator.clone(),
                    update: TotalDeltas(Data { pre, post }),
                });
            } else if let Some(validator) = is_validator_voting_power_key(key) {
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    ValidatorVotingPowers::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    ValidatorVotingPowers::try_from_slice(&bytes[..]).ok()
                });
                changes.push(Validator {
                    address: validator.clone(),
                    update: VotingPowerUpdate(Data { pre, post }),
                });
            } else if let Some(raw_hash) =
                is_validator_address_raw_hash_key(key)
            {
                let pre =
                    self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                        Address::try_from_slice(&bytes[..]).ok()
                    });
                let post =
                    self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                        Address::try_from_slice(&bytes[..]).ok()
                    });
                changes.push(ValidatorAddressRawHash {
                    raw_hash: raw_hash.to_string(),
                    data: Data { pre, post },
                });
            } else if let Some(owner) =
                token::is_balance_key(&staking_token_address(), key)
            {
                if owner != &addr {
                    continue;
                }
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    token::Amount::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    token::Amount::try_from_slice(&bytes[..]).ok()
                });
                changes.push(Balance(Data { pre, post }));
            } else if let Some(bond_id) = is_bond_key(key) {
                let pre =
                    self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                        Bonds::try_from_slice(&bytes[..]).ok()
                    });
                let post =
                    self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                        Bonds::try_from_slice(&bytes[..]).ok()
                    });
                // For bonds, we need to look-up slashes
                let slashes = self
                    .ctx
                    .pre()
                    .read_bytes(&validator_slashes_key(&bond_id.validator))?
                    .and_then(|bytes| Slashes::try_from_slice(&bytes[..]).ok())
                    .unwrap_or_default();
                changes.push(Bond {
                    id: bond_id.clone(),
                    data: Data { pre, post },
                    slashes,
                });
            } else if let Some(unbond_id) = is_unbond_key(key) {
                let pre =
                    self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                        Unbonds::try_from_slice(&bytes[..]).ok()
                    });
                let post =
                    self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                        Unbonds::try_from_slice(&bytes[..]).ok()
                    });
                // For unbonds, we need to look-up slashes
                let slashes = self
                    .ctx
                    .pre()
                    .read_bytes(&validator_slashes_key(&unbond_id.validator))?
                    .and_then(|bytes| Slashes::try_from_slice(&bytes[..]).ok())
                    .unwrap_or_default();
                changes.push(Unbond {
                    id: unbond_id.clone(),
                    data: Data { pre, post },
                    slashes,
                });
            } else if is_total_voting_power_key(key) {
                let pre = self.ctx.pre().read_bytes(key)?.and_then(|bytes| {
                    TotalVotingPowers::try_from_slice(&bytes[..]).ok()
                });
                let post = self.ctx.post().read_bytes(key)?.and_then(|bytes| {
                    TotalVotingPowers::try_from_slice(&bytes[..]).ok()
                });
                changes.push(TotalVotingPower(Data { pre, post }));
            } else if key.segments.get(0) == Some(&addr.to_db_key()) {
                // Unknown changes to this address space are disallowed
                tracing::info!("PoS unrecognized key change {} rejected", key);
                return Ok(false);
            } else {
                // Unknown changes anywhere else are permitted
                return Ok(true);
            }
        }

        let params = self.ctx.pre().read_pos_params()?;
        let errors = validate(&params, changes, current_epoch);
        Ok(if errors.is_empty() {
            true
        } else {
            tracing::info!(
                "PoS validation errors:\n - {}",
                errors.iter().format("\n - ")
            );
            false
        })
    }
}

impl_pos_read_only! {
    type Error = native_vp::Error;
    impl<'f, 'a, DB, H, CA> PosReadOnly for CtxPreStorageRead<'f, 'a, DB, H, CA>
        where
            DB: ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter> +'static,
            H: StorageHasher +'static,
            CA: WasmCacheAccess +'static
}

impl_pos_read_only! {
    type Error = native_vp::Error;
    impl<'f, 'a, DB, H, CA> PosReadOnly for CtxPostStorageRead<'f, 'a, DB, H, CA>
        where
            DB: ledger_storage::DB + for<'iter> ledger_storage::DBIter<'iter> +'static,
            H: StorageHasher +'static,
            CA: WasmCacheAccess +'static
}

impl From<native_vp::Error> for Error {
    fn from(err: native_vp::Error) -> Self {
        Self::NativeVpError(err)
    }
}

impl From<storage_api::Error> for Error {
    fn from(err: storage_api::Error) -> Self {
        Self::StorageApi(err)
    }
}
