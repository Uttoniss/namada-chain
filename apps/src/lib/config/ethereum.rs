//! Configuration settings to do with the Ethereum bridge.
#[allow(unused_imports)]
use namada::types::ethereum_events::EthereumEvent;
use serde::{Deserialize, Serialize};

/// Default [Ethereum JSON-RPC](https://ethereum.org/en/developers/docs/apis/json-rpc/) endpoint used by the oracle
pub const DEFAULT_ORACLE_RPC_ENDPOINT: &str = "http://127.0.0.1:8545";

/// The mode in which to run the Ethereum bridge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Mode {
    /// Run `geth` in a subprocess, exposing an Ethereum
    /// JSON-RPC endpoint at [`DEFAULT_ORACLE_RPC_ENDPOINT`]. By default, the
    /// oracle is configured to listen for events from the Ethereum bridge
    /// smart contracts using this endpoint.
    Managed,
    /// Do not run `geth`. The oracle will listen to the Ethereum JSON-RPC
    /// endpoint as specified in the `oracle_rpc_endpoint` setting.
    Remote,
    /// Do not start a managed `geth` subprocess. Instead of the oracle
    /// listening for events using a Ethereum JSON-RPC endpoint, an endpoint
    /// will be exposed by the ledger itself for submission of Borsh-
    /// serialized [`EthereumEvent`]s. Mostly useful for testing purposes.
    EventsEndpoint,
    /// Do not run any components of the Ethereum bridge.
    Off,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// The mode in which to run the Ethereum bridge
    pub mode: Mode,
    /// The Ethereum JSON-RPC endpoint that the Ethereum event oracle will use
    /// to listen for events from the Ethereum bridge smart contracts
    pub oracle_rpc_endpoint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Managed,
            oracle_rpc_endpoint: DEFAULT_ORACLE_RPC_ENDPOINT.to_owned(),
        }
    }
}
