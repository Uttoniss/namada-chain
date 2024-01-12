//! MASP native VP

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};

use borsh_ext::BorshSerializeExt;
use masp_primitives::merkle_tree::CommitmentTree;
use masp_primitives::sapling::Node;
use masp_primitives::transaction::components::I128Sum;
use masp_primitives::transaction::Transaction;
use namada_core::ledger::gas::MASP_VERIFY_SHIELDED_TX_GAS;
use namada_core::ledger::storage;
use namada_core::ledger::storage_api::OptionExt;
use namada_core::ledger::vp_env::VpEnv;
use namada_core::proto::Tx;
use namada_core::types::address::Address;
use namada_core::types::address::InternalAddress::Masp;
use namada_core::types::storage::{IndexedTx, Key};
use namada_core::types::token::{
    self, is_masp_allowed_key, is_masp_key, is_masp_nullifier_key,
    masp_pin_tx_key,
};
use namada_sdk::masp::verify_shielded_tx;
use ripemd::Digest as RipemdDigest;
use sha2::Digest as Sha2Digest;
use thiserror::Error;

use crate::ledger::native_vp;
use crate::ledger::native_vp::{Ctx, NativeVp};
use crate::vm::WasmCacheAccess;

#[allow(missing_docs)]
#[derive(Error, Debug)]
pub enum Error {
    #[error("Native VP error: {0}")]
    NativeVpError(native_vp::Error),
}

/// MASP VP result
pub type Result<T> = std::result::Result<T, Error>;

/// MASP VP
pub struct MaspVp<'a, DB, H, CA>
where
    DB: storage::DB + for<'iter> storage::DBIter<'iter>,
    H: storage::StorageHasher,
    CA: WasmCacheAccess,
{
    /// Context to interact with the host structures.
    pub ctx: Ctx<'a, DB, H, CA>,
}

impl<'a, DB, H, CA> MaspVp<'a, DB, H, CA>
where
    DB: 'static + storage::DB + for<'iter> storage::DBIter<'iter>,
    H: 'static + storage::StorageHasher,
    CA: 'static + WasmCacheAccess,
{
    // Check that the transaction correctly revealed the nullifiers
    fn valid_nullifiers_reveal(
        &self,
        keys_changed: &[&Key],
        transaction: &Transaction,
    ) -> Result<bool> {
        let mut revealed_nullifiers = HashSet::new();
        let shielded_spends = match transaction.sapling_bundle() {
            Some(bundle) if !bundle.shielded_spends.is_empty() => {
                &bundle.shielded_spends
            }
            _ => {
                tracing::debug!(
                    "Missing expected spend descriptions in shielded \
                     transaction"
                );
                return Ok(false);
            }
        };

        for description in shielded_spends {
            let nullifier_key = namada_core::types::token::masp_nullifier_key(
                &description.nullifier,
            );
            if self.ctx.has_key_pre(&nullifier_key)?
                || revealed_nullifiers.contains(&nullifier_key)
            {
                tracing::debug!(
                    "MASP double spending attempt, the nullifier {:#?} has \
                     already been revealed previously",
                    description.nullifier.0
                );
                return Ok(false);
            }

            // Check that the nullifier is indeed committed (no temp write
            // and no delete) and carries no associated data (the latter not
            // strictly necessary for validation, but we don't expect any
            // value for this key anyway)
            match self.ctx.read_bytes_post(&nullifier_key)? {
                Some(value) if value.is_empty() => (),
                _ => return Ok(false),
            }

            revealed_nullifiers.insert(nullifier_key);
        }

        for nullifier_key in
            keys_changed.iter().filter(|key| is_masp_nullifier_key(key))
        {
            if !revealed_nullifiers.contains(nullifier_key) {
                tracing::debug!(
                    "An unexpected MASP nullifier key {nullifier_key} has \
                     been revealed by the transaction"
                );
                return Ok(false);
            }
        }

        Ok(true)
    }

    // Check that a transaction carrying output descriptions correctly updates
    // the tree and anchor in storage
    fn valid_note_commitment_update(
        &self,
        transaction: &Transaction,
    ) -> Result<bool> {
        // Check that the merkle tree in storage has been correctly updated with
        // the output descriptions cmu
        let tree_key = namada_core::types::token::masp_commitment_tree_key();
        let mut previous_tree: CommitmentTree<Node> =
            self.ctx.read_pre(&tree_key)?.ok_or(Error::NativeVpError(
                native_vp::Error::SimpleMessage("Cannot read storage"),
            ))?;
        let post_tree: CommitmentTree<Node> =
            self.ctx.read_post(&tree_key)?.ok_or(Error::NativeVpError(
                native_vp::Error::SimpleMessage("Cannot read storage"),
            ))?;

        // Based on the output descriptions of the transaction, update the
        // previous tree in storage
        for description in transaction
            .sapling_bundle()
            .map_or(&vec![], |bundle| &bundle.shielded_outputs)
        {
            previous_tree
                .append(Node::from_scalar(description.cmu))
                .map_err(|()| {
                    Error::NativeVpError(native_vp::Error::SimpleMessage(
                        "Failed to update the commitment tree",
                    ))
                })?;
        }
        // Check that the updated previous tree matches the actual post tree.
        // This verifies that all and only the necessary notes have been
        // appended to the tree
        if previous_tree != post_tree {
            tracing::debug!("The note commitment tree was incorrectly updated");
            return Ok(false);
        }

        Ok(true)
    }

    // Check that the spend descriptions anchors of a transaction are valid
    fn valid_spend_descriptions_anchor(
        &self,
        transaction: &Transaction,
    ) -> Result<bool> {
        let shielded_spends = match transaction.sapling_bundle() {
            Some(bundle) if !bundle.shielded_spends.is_empty() => {
                &bundle.shielded_spends
            }
            _ => {
                tracing::debug!(
                    "Missing expected spend descriptions in shielded \
                     transaction"
                );
                return Ok(false);
            }
        };

        for description in shielded_spends {
            let anchor_key =
                namada_core::types::token::masp_commitment_anchor_key(
                    description.anchor,
                );

            // Check if the provided anchor was published before
            if !self.ctx.has_key_pre(&anchor_key)? {
                tracing::debug!(
                    "Spend description refers to an invalid anchor"
                );
                return Ok(false);
            }
        }

        Ok(true)
    }

    // Check that the convert descriptions anchors of a transaction are valid
    fn valid_convert_descriptions_anchor(
        &self,
        transaction: &Transaction,
    ) -> Result<bool> {
        if let Some(bundle) = transaction.sapling_bundle() {
            if !bundle.shielded_converts.is_empty() {
                let anchor_key =
                    namada_core::types::token::masp_convert_anchor_key();
                let expected_anchor = self
                    .ctx
                    .read_pre::<namada_core::types::hash::Hash>(&anchor_key)?
                    .ok_or(Error::NativeVpError(
                        native_vp::Error::SimpleMessage("Cannot read storage"),
                    ))?;

                for description in &bundle.shielded_converts {
                    // Check if the provided anchor matches the current
                    // conversion tree's one
                    if namada_core::types::hash::Hash(
                        description.anchor.to_bytes(),
                    ) != expected_anchor
                    {
                        tracing::debug!(
                            "Convert description refers to an invalid anchor"
                        );
                        return Ok(false);
                    }
                }
            }
        }

        Ok(true)
    }

    fn valid_state(
        &self,
        masp_keys_changed: &[&Key],
        pin_key: Option<&str>,
    ) -> Result<bool> {
        // Check that the transaction didn't write unallowed masp keys
        if masp_keys_changed
            .iter()
            .any(|key| !is_masp_allowed_key(key))
        {
            return Ok(false);
        }

        // Validate pin key
        if let Some(key) = pin_key {
            match self.ctx.read_post::<IndexedTx>(&masp_pin_tx_key(key))? {
                Some(IndexedTx { height, index })
                    if height == self.ctx.get_block_height()?
                        && index == self.ctx.get_tx_index()? => {}
                _ => return Ok(false),
            }
        }

        Ok(true)
    }
}

impl<'a, DB, H, CA> NativeVp for MaspVp<'a, DB, H, CA>
where
    DB: 'static + storage::DB + for<'iter> storage::DBIter<'iter>,
    H: 'static + storage::StorageHasher,
    CA: 'static + WasmCacheAccess,
{
    type Error = Error;

    fn validate_tx(
        &self,
        tx_data: &Tx,
        keys_changed: &BTreeSet<Key>,
        _verifiers: &BTreeSet<Address>,
    ) -> Result<bool> {
        let epoch = self.ctx.get_block_epoch()?;
        let conversion_state = self.ctx.storage.get_conversion_state();
        let (transfer, shielded_tx) = self.ctx.get_shielded_action(tx_data)?;

        if u64::from(self.ctx.get_block_height()?)
            > u64::from(shielded_tx.expiry_height())
        {
            tracing::debug!("MASP transaction is expired");
            return Ok(false);
        }

        let transfer_amount = transfer
            .amount
            .to_amount(&transfer.token, &self.ctx.pre())?;
        let mut transparent_tx_pool = I128Sum::zero();
        // The Sapling value balance adds to the transparent tx pool
        transparent_tx_pool += shielded_tx.sapling_value_balance();

        // Check the validity of the keys
        let masp_keys_changed: Vec<&Key> =
            keys_changed.iter().filter(|key| is_masp_key(key)).collect();
        if !self.valid_state(
            masp_keys_changed.as_slice(),
            transfer.key.as_deref(),
        )? {
            return Ok(false);
        }

        if transfer.source != Address::Internal(Masp) {
            // No shielded spends nor shielded converts are allowed
            if shielded_tx.sapling_bundle().is_some_and(|bundle| {
                !(bundle.shielded_spends.is_empty()
                    && bundle.shielded_converts.is_empty())
            }) {
                return Ok(false);
            }

            let transp_bundle =
                shielded_tx.transparent_bundle().ok_or_err_msg(
                    "Expected transparent outputs in shielding transaction",
                )?;
            let mut total_in_values = token::Amount::zero();
            let source_enc = transfer.source.serialize_to_vec();
            let hash =
                ripemd::Ripemd160::digest(sha2::Sha256::digest(&source_enc));

            // Handle transparent input
            // The following boundary conditions must be satisfied
            // 1. Total of transparent input values equals containing transfer
            // amount 2. Asset type must be properly derived
            // 3. Public key must be the hash of the source
            for vin in &transp_bundle.vin {
                match conversion_state.assets.get(&vin.asset_type) {
                    // Satisfies 2.
                    Some(((address, denom), asset_epoch, _, _))
                        if address == &transfer.token
                            && asset_epoch <= &epoch =>
                    {
                        total_in_values += token::Amount::from_masp_denominated(
                            vin.value, *denom,
                        );

                        // Non-masp sources add to the transparent tx pool
                        transparent_tx_pool += I128Sum::from_nonnegative(
                            vin.asset_type,
                            vin.value as i128,
                        )
                        .ok()
                        .ok_or_err_msg(
                            "invalid value or asset type for amount",
                        )?;

                        // Satisfies 3.
                        if <[u8; 20]>::from(hash) != vin.address.0 {
                            tracing::debug!(
                                "the public key of the output account does \
                                 not match the transfer target"
                            );
                            return Ok(false);
                        }
                    }
                    // unrecognized asset
                    _ => return Ok(false),
                };
            }
            // Satisfies 1.
            if total_in_values != transfer_amount {
                return Ok(false);
            }
        } else {
            // Handle shielded input
            // The following boundary conditions must be satisfied
            // 1. Zero transparent input
            // 2. the transparent transaction value pool's amount must equal
            // the containing wrapper transaction's fee
            // amount Satisfies 1.
            // 3. The spend descriptions' anchors are valid
            // 4. The convert descriptions's anchors are valid
            // 5. The nullifiers provided by the transaction have not been
            // revealed previously (even in the same tx) and no unneeded
            // nullifier is being revealed by the tx
            if let Some(transp_bundle) = shielded_tx.transparent_bundle() {
                if !transp_bundle.vin.is_empty() {
                    tracing::debug!(
                        "Transparent input to a transaction from the masp \
                         must be 0 but is {}",
                        transp_bundle.vin.len()
                    );
                    return Ok(false);
                }
            }

            if !(self.valid_spend_descriptions_anchor(&shielded_tx)?
                && self.valid_convert_descriptions_anchor(&shielded_tx)?
                && self.valid_nullifiers_reveal(
                    &masp_keys_changed,
                    &shielded_tx,
                )?)
            {
                return Ok(false);
            }
        }

        // The transaction must correctly update the note commitment tree
        // in storage with the new output descriptions
        if !self.valid_note_commitment_update(&shielded_tx)? {
            return Ok(false);
        }

        if transfer.target != Address::Internal(Masp) {
            // Handle transparent output
            // The following boundary conditions must be satisfied
            // 1. Total of transparent output values equals containing transfer
            // amount 2. Asset type must be properly derived
            // 3. Public key must be the hash of the target

            let transp_bundle =
                shielded_tx.transparent_bundle().ok_or_err_msg(
                    "Expected transparent outputs in unshielding transaction",
                )?;

            let mut total_out_values = token::Amount::zero();
            let target_enc = transfer.target.serialize_to_vec();
            let hash =
                ripemd::Ripemd160::digest(sha2::Sha256::digest(&target_enc));

            for out in &transp_bundle.vout {
                match conversion_state.assets.get(&out.asset_type) {
                    // Satisfies 2.
                    Some(((address, denom), asset_epoch, _, _))
                        if address == &transfer.token
                            && asset_epoch <= &epoch =>
                    {
                        total_out_values +=
                            token::Amount::from_masp_denominated(
                                out.value, *denom,
                            );

                        // Non-masp destinations subtract from transparent tx
                        // pool
                        transparent_tx_pool -= I128Sum::from_nonnegative(
                            out.asset_type,
                            out.value as i128,
                        )
                        .ok()
                        .ok_or_err_msg(
                            "invalid value or asset type for amount",
                        )?;

                        // Satisfies 3.
                        if <[u8; 20]>::from(hash) != out.address.0 {
                            tracing::debug!(
                                "the public key of the output account does \
                                 not match the transfer target"
                            );
                            return Ok(false);
                        }
                    }
                    // unrecognized asset
                    _ => return Ok(false),
                };
            }
            // Satisfies 1.
            if total_out_values != transfer_amount {
                return Ok(false);
            }
        } else {
            // Handle shielded output
            // The following boundary conditions must be satisfied
            // 1. Zero transparent output
            // 2. At least one shielded output

            // Satisfies 1.
            if let Some(transp_bundle) = shielded_tx.transparent_bundle() {
                if !transp_bundle.vout.is_empty() {
                    tracing::debug!(
                        "Transparent output to a transaction from the masp \
                         must be 0 but is {}",
                        transp_bundle.vout.len()
                    );
                    return Ok(false);
                }
            }

            // Staisfies 2.
            if shielded_tx
                .sapling_bundle()
                .is_some_and(|bundle| bundle.shielded_outputs.is_empty())
            {
                return Ok(false);
            }
        }

        match transparent_tx_pool.partial_cmp(&I128Sum::zero()) {
            None | Some(Ordering::Less) => {
                tracing::debug!(
                    "Transparent transaction value pool must be nonnegative. \
                     Violation may be caused by transaction being constructed \
                     in previous epoch. Maybe try again."
                );
                // Section 3.4: The remaining value in the transparent
                // transaction value pool MUST be nonnegative.
                return Ok(false);
            }
            Some(Ordering::Greater) => {
                tracing::debug!(
                    "Transaction fees cannot be paid inside MASP transaction."
                );
                return Ok(false);
            }
            _ => {}
        }

        // Verify the proofs and charge the gas for the expensive execution
        self.ctx
            .charge_gas(MASP_VERIFY_SHIELDED_TX_GAS)
            .map_err(Error::NativeVpError)?;
        Ok(verify_shielded_tx(&shielded_tx))
    }
}

impl From<native_vp::Error> for Error {
    fn from(err: native_vp::Error) -> Self {
        Self::NativeVpError(err)
    }
}
