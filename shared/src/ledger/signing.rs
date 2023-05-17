//! Functions to sign transactions
use borsh::BorshSerialize;
use namada_core::ledger::parameters::storage as parameter_storage;
use namada_core::types::address::{Address, ImplicitAddress};
use namada_core::types::token::{self, Amount};
use namada_core::types::transaction::MIN_FEE;

use crate::ledger::rpc::TxBroadcastData;
use crate::ledger::tx::Error;
use crate::ledger::wallet::{Wallet, WalletUtils};
use crate::ledger::{args, rpc};
use crate::proto::Tx;
use crate::types::key::*;
use crate::types::storage::Epoch;
use crate::types::transaction::{hash_tx, Fee, WrapperTx};

/// Find the public key for the given address and try to load the keypair
/// for it from the wallet. If the keypair is encrypted but a password is not
/// supplied, then it is interactively prompted. Errors if the key cannot be
/// found or loaded.
pub async fn find_keypair<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    addr: &Address,
    password: Option<String>,
) -> Result<common::SecretKey, Error> {
    match addr {
        Address::Established(_) => {
            println!(
                "Looking-up public key of {} from the ledger...",
                addr.encode()
            );
            let public_key = rpc::get_public_key(client, addr).await.ok_or(
                Error::Other(format!(
                    "No public key found for the address {}",
                    addr.encode()
                )),
            )?;
            wallet.find_key_by_pk(&public_key, password).map_err(|err| {
                Error::Other(format!(
                    "Unable to load the keypair from the wallet for public \
                     key {}. Failed with: {}",
                    public_key, err
                ))
            })
        }
        Address::Implicit(ImplicitAddress(pkh)) => {
            wallet.find_key_by_pkh(pkh, password).map_err(|err| {
                Error::Other(format!(
                    "Unable to load the keypair from the wallet for the \
                     implicit address {}. Failed with: {}",
                    addr.encode(),
                    err
                ))
            })
        }
        Address::Internal(_) => other_err(format!(
            "Internal address {} doesn't have any signing keys.",
            addr
        )),
    }
}

/// Carries types that can be directly/indirectly used to sign a transaction.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum TxSigningKey {
    /// Do not sign any transaction
    None,
    /// Obtain the actual keypair from wallet and use that to sign
    WalletKeypair(common::SecretKey),
    /// Obtain the keypair corresponding to given address from wallet and sign
    WalletAddress(Address),
    /// Directly use the given secret key to sign transactions
    SecretKey(common::SecretKey),
}

/// Given CLI arguments and some defaults, determine the rightful transaction
/// signer. Return the given signing key or public key of the given signer if
/// possible. If no explicit signer given, use the `default`. If no `default`
/// is given, an `Error` is returned.
pub async fn tx_signer<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    args: &args::Tx,
    default: TxSigningKey,
) -> Result<common::SecretKey, Error> {
    // Override the default signing key source if possible
    let default = if let Some(signing_key) = &args.signing_key {
        TxSigningKey::WalletKeypair(signing_key.clone())
    } else if let Some(signer) = &args.signer {
        TxSigningKey::WalletAddress(signer.clone())
    } else {
        default
    };
    // Now actually fetch the signing key and apply it
    match default {
        TxSigningKey::WalletKeypair(signing_key) => Ok(signing_key),
        TxSigningKey::WalletAddress(signer) => {
            let signer = signer;
            let signing_key = find_keypair::<C, U>(
                client,
                wallet,
                &signer,
                args.password.clone(),
            )
            .await?;
            // Check if the signer is implicit account that needs to reveal its
            // PK first
            if matches!(signer, Address::Implicit(_)) {
                let pk: common::PublicKey = signing_key.ref_to();
                super::tx::reveal_pk_if_needed::<C, U>(
                    client, wallet, &pk, args,
                )
                .await?;
            }
            Ok(signing_key)
        }
        TxSigningKey::SecretKey(signing_key) => {
            // Check if the signing key needs to reveal its PK first
            let pk: common::PublicKey = signing_key.ref_to();
            super::tx::reveal_pk_if_needed::<C, U>(client, wallet, &pk, args)
                .await?;
            Ok(signing_key)
        }
        TxSigningKey::None => other_err(
            "All transactions must be signed; please either specify the key \
             or the address from which to look up the signing key."
                .to_string(),
        ),
    }
}

/// Sign a transaction with a given signing key or public key of a given signer.
/// If no explicit signer given, use the `default`. If no `default` is given,
/// Error.
///
/// If this is not a dry run, the tx is put in a wrapper and returned along with
/// hashes needed for monitoring the tx on chain.
///
/// If it is a dry run, it is not put in a wrapper, but returned as is.
pub async fn sign_tx<
    C: crate::ledger::queries::Client + Sync,
    U: WalletUtils,
>(
    client: &C,
    wallet: &mut Wallet<U>,
    tx: Tx,
    args: &args::Tx,
    default: TxSigningKey,
    #[cfg(not(feature = "mainnet"))] requires_pow: bool,
) -> Result<TxBroadcastData, Error> {
    let keypair = tx_signer::<C, U>(client, wallet, args, default).await?;
    let tx = tx.sign(&keypair);

    let epoch = rpc::query_epoch(client).await;

    let broadcast_data = if args.dry_run {
        TxBroadcastData::DryRun(tx)
    } else {
        sign_wrapper(
            client,
            args,
            epoch,
            tx,
            &keypair,
            #[cfg(not(feature = "mainnet"))]
            requires_pow,
        )
        .await
    };

    Ok(broadcast_data)
}

/// Create a wrapper tx from a normal tx. Get the hash of the
/// wrapper and its payload which is needed for monitoring its
/// progress on chain.
pub async fn sign_wrapper<C: crate::ledger::queries::Client + Sync>(
    client: &C,
    args: &args::Tx,
    epoch: Epoch,
    tx: Tx,
    keypair: &common::SecretKey,
    #[cfg(not(feature = "mainnet"))] requires_pow: bool,
) -> TxBroadcastData {
    let fee_amount = if cfg!(feature = "mainnet") {
        Amount::whole(MIN_FEE)
    } else {
        let wrapper_tx_fees_key = parameter_storage::get_wrapper_tx_fees_key();
        rpc::query_storage_value::<C, token::Amount>(
            client,
            &wrapper_tx_fees_key,
        )
        .await
        .unwrap_or_default()
    };
    let fee_token = args.fee_token.clone();
    let source = Address::from(&keypair.ref_to());
    let balance_key = token::balance_key(&fee_token, &source);
    let balance =
        rpc::query_storage_value::<C, token::Amount>(client, &balance_key)
            .await
            .unwrap_or_default();

    // todo: provide sdk clients an error if the fee balance is insufficient

    #[cfg(not(feature = "mainnet"))]
    // A PoW solution can be used to allow zero-fee testnet transactions
    let pow_solution: Option<namada_core::ledger::testnet_pow::Solution> = {
        // If the address derived from the keypair doesn't have enough balance
        // to pay for the fee, allow to find a PoW solution instead.
        if requires_pow || balance < fee_amount {
            println!(
                "The transaction requires the completion of a PoW challenge."
            );
            // Obtain a PoW challenge for faucet withdrawal
            let challenge =
                rpc::get_testnet_pow_challenge(client, source).await;

            // Solve the solution, this blocks until a solution is found
            let solution = challenge.solve();
            Some(solution)
        } else {
            None
        }
    };

    let tx = {
        WrapperTx::new(
            Fee {
                amount: fee_amount,
                token: fee_token,
            },
            keypair,
            epoch,
            args.gas_limit.clone(),
            tx,
            // TODO: Actually use the fetched encryption key
            Default::default(),
            #[cfg(not(feature = "mainnet"))]
            pow_solution,
        )
    };

    // We use this to determine when the wrapper tx makes it on-chain
    let wrapper_hash = hash_tx(&tx.try_to_vec().unwrap()).to_string();
    // We use this to determine when the decrypted inner tx makes it
    // on-chain
    let decrypted_hash = tx.tx_hash.to_string();
    TxBroadcastData::Wrapper {
        tx: tx
            .sign(keypair, args.chain_id.clone().unwrap(), args.expiration)
            .expect("Wrapper tx signing keypair should be correct"),
        wrapper_hash,
        decrypted_hash,
    }
}

fn other_err<T>(string: String) -> Result<T, Error> {
    Err(Error::Other(string))
}
