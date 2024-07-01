#![no_main]

use lazy_static::lazy_static;
use libfuzzer_sys::fuzz_target;
use namada_node::shell;
use namada_node::shell::test_utils::TestShell;
use namada_node::shell::MempoolTxType;
use namada_tx::Tx;

lazy_static! {
    static ref SHELL: TestShell = {
        let (shell, _recv, _, _) = shell::test_utils::setup();
        shell
    };
}

fuzz_target!(|tx: Tx| {
    SHELL.mempool_validate(&tx.to_bytes(), MempoolTxType::NewTransaction);
});
