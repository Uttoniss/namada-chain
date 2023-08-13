// cd shared && cargo expand ledger::queries::vp::governance

use namada_core::ledger::governance::storage::proposal::StorageProposal;
use namada_core::ledger::governance::utils::Vote;

use crate::ledger::queries::types::RequestCtx;
use crate::ledger::storage::{DBIter, StorageHasher, DB};
use crate::ledger::storage_api;

// Governance queries
router! {GOV,
    ( "proposal" / [id: u64 ] ) -> Option<StorageProposal> = proposal_id,
    ( "proposal" / [id: u64 ] / "votes" ) -> Vec<Vote> = proposal_id_votes,
}

/// Find if the given address belongs to a validator account.
fn proposal_id<D, H>(
    ctx: RequestCtx<'_, D, H>,
    id: u64,
) -> storage_api::Result<Option<StorageProposal>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    storage_api::governance::get_proposal_by_id(ctx.wl_storage, id)
}

/// Find if the given address belongs to a validator account.
fn proposal_id_votes<D, H>(
    ctx: RequestCtx<'_, D, H>,
    id: u64,
) -> storage_api::Result<Vec<Vote>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    storage_api::governance::get_proposal_votes(ctx.wl_storage, id)
}
