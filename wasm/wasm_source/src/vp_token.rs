//! A VP for a fungible token. Enforces that the total supply is unchanged in a
//! transaction that moves balance(s).

use std::collections::BTreeSet;

use namada_vp_prelude::address::{self, Address};
use namada_vp_prelude::storage::KeySeg;
use namada_vp_prelude::{storage, token, *};

#[validity_predicate]
fn validate_tx(
    ctx: &Ctx,
    tx_data: Tx,
    addr: Address,
    keys_changed: BTreeSet<storage::Key>,
    verifiers: BTreeSet<Address>,
) -> VpResult {
    debug_log!(
        "validate_tx called with token addr: {}, key_changed: {:?}, \
         verifiers: {:?}",
        addr,
        keys_changed,
        verifiers
    );

    if !is_valid_tx(ctx, &tx_data)? {
        return reject();
    }

    for key in keys_changed.iter() {
        if key.is_validity_predicate().is_some() {
            let vp_hash: Vec<u8> = ctx.read_bytes_post(key)?.unwrap();
            if !is_vp_whitelisted(ctx, &vp_hash)? {
                return reject();
            }
        }
    }

    token_checks(ctx, &addr, &keys_changed, &verifiers)
}

/// A token validity predicate checks that the total supply is preserved.
/// This implies that:
///
/// - The value associated with the `total_supply` storage key may not change.
/// - For any balance changes, the total of outputs must be equal to the total
///   of inputs.
fn token_checks(
    ctx: &Ctx,
    token: &Address,
    keys_touched: &BTreeSet<storage::Key>,
    verifiers: &BTreeSet<Address>,
) -> VpResult {
    for key in keys_touched.iter() {
        let owner: Option<&Address> = token::is_balance_key(token, key);

        match owner {
            None => {
                if key.segments.get(0) == Some(&token.to_db_key()) {
                    // Unknown changes to this address space are disallowed, but
                    // unknown changes anywhere else are permitted
                    return reject();
                }
            }
            Some(owner) => {
                if token::is_minted_balance_key(token, key) {
                    // check if total supply is changed, which it should never
                    // be from a tx
                    let total_pre: token::Amount = ctx.read_pre(key)?.unwrap();
                    let total_post: token::Amount =
                        ctx.read_post(key)?.unwrap();
                    if total_pre != total_post {
                        return reject();
                    }
                } else {
                    // accumulate the change
                    let pre: token::Amount =
                        ctx.read_pre(key)?.unwrap_or_default();
                    let post: token::Amount =
                        ctx.read_post(key)?.unwrap_or_default();
                    // make sure that the spender approved the transaction
                    if post < pre
                        && !(verifiers.contains(owner)
                            || *owner == address::masp())
                    {
                        return reject();
                    }
                }
            }
        }
    }
    // The total change should be validated by multitoken VP
    Ok(true)
}

#[cfg(test)]
mod tests {
    // Use this as `#[test]` annotation to enable logging
    use namada::core::ledger::storage_api::token;
    use namada::proto::Data;
    use namada::types::transaction::TxType;
    use namada_tests::log::test;
    use namada_tests::tx::{self, TestTxEnv};
    use namada_tests::vp::*;
    use namada_vp_prelude::storage_api::StorageWrite;

    use super::*;

    #[test]
    fn test_transfer_inputs_eq_outputs_is_accepted() {
        // Initialize a tx environment
        let mut tx_env = TestTxEnv::default();
        let token = address::nam();
        let src = address::testing::established_address_1();
        let dest = address::testing::established_address_2();
        let total_supply = token::Amount::from(10_098_123);

        // Spawn the accounts to be able to modify their storage
        tx_env.spawn_accounts([&token, &src, &dest]);
        token::credit_tokens(
            &mut tx_env.wl_storage,
            &token,
            &src,
            total_supply,
        )
        .unwrap();
        // Commit the initial state
        tx_env.commit_tx_and_block();

        // Initialize VP environment from a transaction
        vp_host_env::init_from_tx(token.clone(), tx_env, |_address| {
            // Apply a transfer

            let amount = token::Amount::from(100);
            token::transfer(tx::ctx(), &token, &src, &dest, amount).unwrap();
        });

        let vp_env = vp_host_env::take();
        let mut tx_data = Tx::new(TxType::Raw);
        tx_data.set_data(Data::new(vec![]));
        let keys_changed: BTreeSet<storage::Key> =
            vp_env.all_touched_storage_keys();
        let verifiers = vp_env.get_verifiers();
        vp_host_env::set(vp_env);

        assert!(
            validate_tx(&CTX, tx_data, token, keys_changed, verifiers).unwrap(),
            "A transfer where inputs == outputs should be accepted"
        );
    }

    #[test]
    fn test_transfer_inputs_neq_outputs_is_rejected() {
        // Initialize a tx environment
        let mut tx_env = TestTxEnv::default();
        let token = address::nam();
        let src = address::testing::established_address_1();
        let dest = address::testing::established_address_2();
        let total_supply = token::Amount::from(10_098_123);

        // Spawn the accounts to be able to modify their storage
        tx_env.spawn_accounts([&token, &src, &dest]);
        token::credit_tokens(
            &mut tx_env.wl_storage,
            &token,
            &src,
            total_supply,
        )
        .unwrap();
        // Commit the initial state
        tx_env.commit_tx_and_block();

        // Initialize VP environment from a transaction
        vp_host_env::init_from_tx(token.clone(), tx_env, |_address| {
            // Apply a transfer

            let amount_in = token::Amount::from(100);
            let amount_out = token::Amount::from(900);

            let src_key = token::balance_key(&token, &src);
            let src_balance =
                token::read_balance(tx::ctx(), &token, &src).unwrap();
            let new_src_balance = src_balance + amount_out;
            let dest_key = token::balance_key(&token, &dest);
            let dest_balance =
                token::read_balance(tx::ctx(), &token, &dest).unwrap();
            let new_dest_balance = dest_balance + amount_in;
            tx::ctx().write(&src_key, new_src_balance).unwrap();
            tx::ctx().write(&dest_key, new_dest_balance).unwrap();
        });

        let vp_env = vp_host_env::take();
        let mut tx_data = Tx::new(TxType::Raw);
        tx_data.set_data(Data::new(vec![]));
        let keys_changed: BTreeSet<storage::Key> =
            vp_env.all_touched_storage_keys();
        let verifiers = vp_env.get_verifiers();
        vp_host_env::set(vp_env);

        assert!(
            !validate_tx(&CTX, tx_data, token, keys_changed, verifiers)
                .unwrap(),
            "A transfer where inputs != outputs should be rejected"
        );
    }

    #[test]
    fn test_total_supply_change_is_rejected() {
        // Initialize a tx environment
        let mut tx_env = TestTxEnv::default();
        let token = address::nam();
        let owner = address::testing::established_address_1();
        let total_supply = token::Amount::from(10_098_123);

        // Spawn the accounts to be able to modify their storage
        tx_env.spawn_accounts([&token, &owner]);
        token::credit_tokens(
            &mut tx_env.wl_storage,
            &token,
            &owner,
            total_supply,
        )
        .unwrap();
        // Commit the initial state
        tx_env.commit_tx_and_block();

        let total_supply_key = token::minted_balance_key(&token);

        // Initialize VP environment from a transaction
        vp_host_env::init_from_tx(token.clone(), tx_env, |_address| {
            // Try to change total supply from a tx

            let current_supply = tx::ctx()
                .read::<token::Amount>(&total_supply_key)
                .unwrap()
                .unwrap_or_default();
            tx::ctx()
                .write(
                    &total_supply_key,
                    current_supply + token::Amount::from(1),
                )
                .unwrap();
        });

        let vp_env = vp_host_env::take();
        let mut tx_data = Tx::new(TxType::Raw);
        tx_data.set_data(Data::new(vec![]));
        let keys_changed: BTreeSet<storage::Key> =
            vp_env.all_touched_storage_keys();
        let verifiers = vp_env.get_verifiers();
        vp_host_env::set(vp_env);

        assert!(
            !validate_tx(&CTX, tx_data, token, keys_changed, verifiers)
                .unwrap(),
            "Change of a `total_supply` value should be rejected"
        );
    }
}
