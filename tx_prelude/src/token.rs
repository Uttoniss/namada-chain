pub use namada_core::ledger::masp_utils;
use namada_core::types::address::Address;
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
    amount: DenominatedAmount,
) -> TxResult {
    if amount.amount != Amount::default() && src != dest {
        let src_key = token::balance_key(token, src);
        let dest_key = token::balance_key(token, dest);
        let src_bal: Option<Amount> = ctx.read(&src_key)?;
        let mut src_bal = src_bal.unwrap_or_else(|| {
            log_string(format!("src {} has no balance", src_key));
            unreachable!()
        });
        src_bal.spend(&amount.amount);
        let mut dest_bal: Amount = ctx.read(&dest_key)?.unwrap_or_default();
        dest_bal.receive(&amount.amount);
        ctx.write(&src_key, src_bal)?;
        ctx.write(&dest_key, dest_bal)?;
    }
    Ok(())
}

/// Mint that can be used in a transaction.
pub fn mint(
    ctx: &mut Ctx,
    minter: &Address,
    target: &Address,
    token: &Address,
    amount: Amount,
) -> TxResult {
    let target_key = token::balance_key(token, target);
    let mut target_bal: Amount = ctx.read(&target_key)?.unwrap_or_default();
    target_bal.receive(&amount);

    let minted_key = token::minted_balance_key(token);
    let mut minted_bal: Amount = ctx.read(&minted_key)?.unwrap_or_default();
    minted_bal.receive(&amount);

    ctx.write(&target_key, target_bal)?;
    ctx.write(&minted_key, minted_bal)?;

    let minter_key = token::minter_key(token);
    ctx.write(&minter_key, minter)?;

    Ok(())
}

/// Burn that can be used in a transaction.
pub fn burn(
    ctx: &mut Ctx,
    target: &Address,
    token: &Address,
    amount: Amount,
) -> TxResult {
    let target_key = token::balance_key(token, target);
    let mut target_bal: Amount = ctx.read(&target_key)?.unwrap_or_default();
    target_bal.spend(&amount);

    // burn the minted amount
    let minted_key = token::minted_balance_key(token);
    let mut minted_bal: Amount = ctx.read(&minted_key)?.unwrap_or_default();
    minted_bal.spend(&amount);

    ctx.write(&target_key, target_bal)?;
    ctx.write(&minted_key, minted_bal)?;

    Ok(())
}
