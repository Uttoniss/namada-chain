//! Functionality for accessing the storage subspace
pub mod eth_bridge_queries;
pub mod parameters;
pub mod proof;
pub mod vote_tallies;
pub mod vp;
pub use namada_core::ledger::eth_bridge::storage::{
    bridge_pool, wrapped_erc20s, *,
};
