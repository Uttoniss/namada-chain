use namada_core::ledger::storage::{DBIter, StorageHasher, DB};
use namada_core::ledger::storage_api;
use namada_core::ledger::storage_api::token::read_denom;
use namada_core::types::address::Address;
use namada_core::types::storage::Key;
use namada_core::types::token;

use crate::ledger::queries::RequestCtx;

router! {TOKEN,
    ( "denomination" / [addr: Address] / [sub_prefix: opt Key] ) -> Option<token::Denomination> = denomination,
    ( "denomination" / [addr: Address] / "ibc" / [_ibc_junk: String] ) -> Option<token::Denomination> = denomination_ibc,
}

/// Get the number of decimal places (in base 10) for a
/// token specified by `addr`.
fn denomination<D, H>(
    ctx: RequestCtx<'_, D, H>,
    addr: Address,
    sub_prefix: Option<Key>,
) -> storage_api::Result<Option<token::Denomination>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    read_denom(ctx.wl_storage, &addr, sub_prefix.as_ref())
}

// TODO Please fix this

/// Get the number of decimal places (in base 10) for a
/// token specified by `addr`.
fn denomination_ibc<D, H>(
    ctx: RequestCtx<'_, D, H>,
    addr: Address,
    _ibc_junk: String,
) -> storage_api::Result<Option<token::Denomination>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    read_denom(ctx.wl_storage, &addr, None)
}
