use std::str::FromStr;

use namada_core::ledger::storage::LastBlock;
use namada_core::types::address::Address;
use namada_core::types::storage::BlockResults;
use namada_core::types::token;
use namada_sdk::error::Error;
use namada_sdk::queries::RPC;
use namada_sdk::rpc;
use tendermint_config::net::Address as TendermintAddress;
use tendermint_rpc::HttpClient;
use tokio::runtime::Runtime;

pub mod account;
pub mod governance;
pub mod pgf;
pub mod pos;
pub mod tx;

/// Query the address of the native token
pub fn query_native_token(tendermint_addr: &str) -> Result<Address, Error> {
    let client = HttpClient::new(
        TendermintAddress::from_str(tendermint_addr)
            .map_err(|e| Error::Other(e.to_string()))?,
    )
    .map_err(|e| Error::Other(e.to_string()))?;
    let rt = Runtime::new().unwrap();
    rt.block_on(rpc::query_native_token(&client))
}

/// Query the last committed block, if any.
pub fn query_block(tendermint_addr: &str) -> Result<Option<LastBlock>, Error> {
    let client = HttpClient::new(
        TendermintAddress::from_str(tendermint_addr)
            .map_err(|e| Error::Other(e.to_string()))?,
    )
    .map_err(|e| Error::Other(e.to_string()))?;
    let rt = Runtime::new().unwrap();
    rt.block_on(rpc::query_block(&client))
}

/// Query the results of the last committed block
pub fn query_results(
    tendermint_addr: &str,
) -> Result<Vec<BlockResults>, Error> {
    let client = HttpClient::new(
        TendermintAddress::from_str(tendermint_addr)
            .map_err(|e| Error::Other(e.to_string()))?,
    )
    .map_err(|e| Error::Other(e.to_string()))?;
    let rt = Runtime::new().unwrap();
    rt.block_on(rpc::query_results(&client))
}
