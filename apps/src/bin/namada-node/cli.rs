//! Namada node CLI.

use eyre::{Context, Result};
use namada::types::time::{DateTimeUtc, Utc};
use namada_apps::cli::{self, cmds};
use namada_apps::node::ledger;

pub fn main() -> Result<()> {
    let (cmd, mut ctx) = cli::namada_node_cli()?;
    if let Some(mode) = ctx.global_args.mode.clone() {
        ctx.config.ledger.cometbft.tendermint_mode = mode;
    }
    match cmd {
        cmds::NamadaNode::Ledger(sub) => match sub {
            cmds::Ledger::Run(cmds::LedgerRun(args)) => {
                let wasm_dir = ctx.wasm_dir();
                sleep_until(args.start_time);
                ctx.config.ledger.cometbft.tx_index = args.tx_index;
                ledger::run(ctx.config.ledger, wasm_dir);
            }
            cmds::Ledger::RunUntil(cmds::LedgerRunUntil(args)) => {
                let wasm_dir = ctx.wasm_dir();
                sleep_until(args.time);
                ctx.config.ledger.shell.action_at_height =
                    Some(args.action_at_height);
                ledger::run(ctx.config.ledger, wasm_dir);
            }
            cmds::Ledger::Reset(_) => {
                ledger::reset(ctx.config.ledger)
                    .wrap_err("Failed to reset Namada node")?;
            }
            cmds::Ledger::DumpDb(cmds::LedgerDumpDb(args)) => {
                ledger::dump_db(ctx.config.ledger, args);
            }
            cmds::Ledger::RollBack(_) => {
                ledger::rollback(ctx.config.ledger)
                    .wrap_err("Failed to rollback the Namada node")?;
            }
        },
        cmds::NamadaNode::Config(sub) => match sub {
            cmds::Config::Gen(cmds::ConfigGen) => {
                // If the config doesn't exit, it gets generated in the context.
                // In here, we just need to overwrite the default chain ID, in
                // case it's been already set to a different value
                if let Some(chain_id) = ctx.global_args.chain_id.as_ref() {
                    ctx.global_config.default_chain_id = chain_id.clone();
                    ctx.global_config
                        .write(&ctx.global_args.base_dir)
                        .unwrap_or_else(|err| {
                            eprintln!("Error writing global config: {}", err);
                            cli::safe_exit(1)
                        });
                }
                tracing::debug!(
                    "Generated config and set default chain ID to {}",
                    &ctx.global_config.default_chain_id
                );
            }
        },
    }
    Ok(())
}

/// Sleep until the given start time if necessary.
fn sleep_until(time: Option<DateTimeUtc>) {
    // Sleep until start time if needed
    if let Some(time) = time {
        if let Ok(sleep_time) =
            time.0.signed_duration_since(Utc::now()).to_std()
        {
            if !sleep_time.is_zero() {
                tracing::info!(
                    "Waiting ledger start time: {:?}, time left: {:?}",
                    time,
                    sleep_time
                );
                std::thread::sleep(sleep_time)
            }
        }
    }
}
