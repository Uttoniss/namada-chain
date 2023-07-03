use masp_primitives::transaction::Transaction;
use namada_core::types::address::{Address, InternalAddress};
use namada_core::types::hash::Hash;
use namada_core::types::storage::KeySeg;
use namada_core::types::token;
pub use namada_core::types::token::*;

use super::*;

#[allow(clippy::too_many_arguments)]
/// A token transfer that can be used in a transaction.
pub fn transfer(
    ctx: &mut Ctx,
    src: &Address,
    dest: &Address,
    token: &Address,
    sub_prefix: Option<storage::Key>,
    amount: DenominatedAmount,
    key: &Option<String>,
    shielded_hash: &Option<Hash>,
    shielded: &Option<Transaction>,
) -> TxResult {
    if amount.amount != Amount::default() {
        let src_key = match &sub_prefix {
            Some(sub_prefix) => {
                let prefix =
                    token::multitoken_balance_prefix(token, sub_prefix);
                token::multitoken_balance_key(&prefix, src)
            }
            None => token::balance_key(token, src),
        };
        let dest_key = match &sub_prefix {
            Some(sub_prefix) => {
                let prefix =
                    token::multitoken_balance_prefix(token, sub_prefix);
                token::multitoken_balance_key(&prefix, dest)
            }
            None => token::balance_key(token, dest),
        };
        if src != dest {
            let src_bal: Option<Amount> = match src {
                Address::Internal(InternalAddress::IbcMint) => {
                    Some(Amount::max_signed())
                }
                Address::Internal(InternalAddress::IbcBurn) => {
                    log_string("invalid transfer from the burn address");
                    unreachable!()
                }
                _ => ctx.read(&src_key)?,
            };
            let mut src_bal = src_bal.unwrap_or_else(|| {
                log_string(format!("src {} has no balance", src_key));
                unreachable!()
            });
            src_bal.spend(&amount.amount);
            let mut dest_bal: Amount = match dest {
                Address::Internal(InternalAddress::IbcMint) => {
                    log_string("invalid transfer to the mint address");
                    unreachable!()
                }
                _ => ctx.read(&dest_key)?.unwrap_or_default(),
            };
            dest_bal.receive(&amount.amount);

            match src {
                Address::Internal(InternalAddress::IbcMint) => {
                    ctx.write_temp(&src_key, src_bal)?;
                }
                Address::Internal(InternalAddress::IbcBurn) => {
                    log_string("invalid transfer from the burn address");
                    unreachable!()
                }
                _ => {
                    ctx.write(&src_key, src_bal)?;
                }
            }
            match dest {
                Address::Internal(InternalAddress::IbcMint) => {
                    log_string("invalid transfer to the mint address");
                    unreachable!()
                }
                Address::Internal(InternalAddress::IbcBurn) => {
                    ctx.write_temp(&dest_key, dest_bal)?;
                }
                _ => {
                    ctx.write(&dest_key, dest_bal)?;
                }
            }
        }
    }

    // If this transaction has a shielded component, then handle it
    // separately
    if let Some(shielded) = shielded {
        let masp_addr = address::masp();
        ctx.insert_verifier(&masp_addr)?;
        let head_tx_key = storage::Key::from(masp_addr.to_db_key())
            .push(&HEAD_TX_KEY.to_owned())
            .expect("Cannot obtain a storage key");
        let current_tx_idx: u64 =
            ctx.read(&head_tx_key).unwrap_or(None).unwrap_or(0);
        let current_tx_key = storage::Key::from(masp_addr.to_db_key())
            .push(&(TX_KEY_PREFIX.to_owned() + &current_tx_idx.to_string()))
            .expect("Cannot obtain a storage key");
        // Save the Transfer object and its location within the blockchain
        // so that clients do not have to separately look these
        // up
        let transfer = Transfer {
            source: src.clone(),
            target: dest.clone(),
            token: token.clone(),
            // todo: build asset types for multitokens
            sub_prefix: None,
            amount,
            key: key.clone(),
            shielded: *shielded_hash,
        };
        let record: (Epoch, BlockHeight, TxIndex, Transfer, Transaction) = (
            ctx.get_block_epoch()?,
            ctx.get_block_height()?,
            ctx.get_tx_index()?,
            transfer,
            shielded.clone(),
        );
        ctx.write(&current_tx_key, record)?;
        ctx.write(&head_tx_key, current_tx_idx + 1)?;
        // If storage key has been supplied, then pin this transaction to it
        if let Some(key) = key {
            let pin_key = storage::Key::from(masp_addr.to_db_key())
                .push(&(PIN_KEY_PREFIX.to_owned() + key))
                .expect("Cannot obtain a storage key");
            ctx.write(&pin_key, current_tx_idx)?;
        }
    }
    Ok(())
}

/// A token transfer with storage keys that can be used in a transaction.
pub fn transfer_with_keys(
    ctx: &mut Ctx,
    src_key: &storage::Key,
    dest_key: &storage::Key,
    amount: Amount,
) -> TxResult {
    let src_owner = is_any_token_or_multitoken_balance_key(src_key);
    let src_bal: Option<Amount> = match src_owner {
        Some([_, Address::Internal(InternalAddress::IbcMint)]) => {
            Some(Amount::max_signed())
        }
        Some([_, Address::Internal(InternalAddress::IbcBurn)]) => {
            log_string("invalid transfer from the burn address");
            unreachable!()
        }
        _ => ctx.read(src_key)?,
    };
    let mut src_bal = src_bal.unwrap_or_else(|| {
        log_string(format!("src {} has no balance", src_key));
        unreachable!()
    });
    src_bal.spend(&amount);
    let dest_owner = is_any_token_balance_key(dest_key);
    let mut dest_bal: Amount = match dest_owner {
        Some([_, Address::Internal(InternalAddress::IbcMint)]) => {
            log_string("invalid transfer to the mint address");
            unreachable!()
        }
        _ => ctx.read(dest_key)?.unwrap_or_default(),
    };
    dest_bal.receive(&amount);
    match src_owner {
        Some([_, Address::Internal(InternalAddress::IbcMint)]) => {
            ctx.write_temp(src_key, src_bal)?;
        }
        _ => ctx.write(src_key, src_bal)?,
    }
    match dest_owner {
        Some([_, Address::Internal(InternalAddress::IbcBurn)]) => {
            ctx.write_temp(dest_key, dest_bal)?;
        }
        _ => ctx.write(dest_key, dest_bal)?,
    }
    Ok(())
}
