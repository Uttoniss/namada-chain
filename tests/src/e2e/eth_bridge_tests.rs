use crate::e2e::helpers::get_actor_rpc;
use crate::e2e::setup;
use crate::e2e::setup::constants::{
    wasm_abs_path, ALBERT, TX_WRITE_STORAGE_KEY_WASM,
};
use crate::e2e::setup::{Bin, Who};
use crate::{run, run_as};

/// this is the predictable bech32m address of the internal EthBridge account
const ETH_BRIDGE_ADDRESS: &str = "atest1v9hx7w36g42ysgzzwf5kgem9ypqkgerjv4ehxgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpq8f99ew";

/// # Examples
///
/// ```
/// let storage_key = storage_key("ERC20/0x6B175474E89094C44Da98b954EedeAC495271d0F/supply");
/// assert_eq!(storage_key, "#atest1v9hx7w36g42ysgzzwf5kgem9ypqkgerjv4ehxgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpq8f99ew/ERC20/0x6B175474E89094C44Da98b954EedeAC495271d0F/supply");
/// ```
fn storage_key(path: &str) -> String {
    format!("#{ETH_BRIDGE_ADDRESS}/{}", path)
}

#[test]
fn unauthorized_tx_cannot_write_storage() {
    const LEDGER_STARTUP_TIMEOUT_SECONDS: u64 = 60;
    const CLIENT_COMMAND_TIMEOUT_SECONDS: u64 = 60;
    const SOLE_VALIDATOR: Who = Who::Validator(0);

    let test = setup::single_node_net().unwrap();

    let mut anoman_ledger = run_as!(
        test,
        SOLE_VALIDATOR,
        Bin::Node,
        &["ledger"],
        Some(LEDGER_STARTUP_TIMEOUT_SECONDS)
    )
    .unwrap();
    anoman_ledger
        .exp_string("Anoma ledger node started")
        .unwrap();
    anoman_ledger.exp_string("Tendermint node started").unwrap();
    anoman_ledger.exp_string("Committed block hash").unwrap();
    let _bg_ledger = anoman_ledger.background();

    let tx_data_path = test.test_dir.path().join("dai_supply_storage_key.txt");
    std::fs::write(
        &tx_data_path,
        &storage_key("ERC20/0x6B175474E89094C44Da98b954EedeAC495271d0F/supply")
            [..],
    )
    .unwrap();

    let tx_code_path = wasm_abs_path(TX_WRITE_STORAGE_KEY_WASM);

    let tx_data_path = tx_data_path.to_string_lossy().to_string();
    let tx_code_path = tx_code_path.to_string_lossy().to_string();
    let ledger_addr = get_actor_rpc(&test, &SOLE_VALIDATOR);
    let tx_args = vec![
        "tx",
        "--signer",
        ALBERT,
        "--code-path",
        &tx_code_path,
        "--data-path",
        &tx_data_path,
        "--ledger-address",
        &ledger_addr,
    ];

    let mut anomac_tx = run!(
        test,
        Bin::Client,
        tx_args,
        Some(CLIENT_COMMAND_TIMEOUT_SECONDS)
    )
    .unwrap();

    if !cfg!(feature = "ABCI") {
        anomac_tx.exp_string("Transaction accepted").unwrap();
    }
    anomac_tx.exp_string("Transaction applied").unwrap();
    // TODO: we should check here explicitly with the ledger via a
    //  Tendermint RPC call that the path `value/#EthBridge/queue`
    //  is unchanged rather than relying solely on looking at anomac
    //  stdout.
    anomac_tx.exp_string("Transaction is invalid").unwrap();
    anomac_tx
        .exp_string(&format!("Rejected: {}", ETH_BRIDGE_ADDRESS))
        .unwrap();
    anomac_tx.assert_success();
}
