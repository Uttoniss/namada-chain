use std::fs;
use std::io::prelude::*;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(not(feature = "dev"))]
use std::str::FromStr;

use ark_std::rand::prelude::*;
use ark_std::rand::SeedableRng;
use file_lock::{FileLock, FileOptions};
#[cfg(not(feature = "dev"))]
use namada::ledger::wallet::store::AddressVpType;
#[cfg(feature = "dev")]
use namada::ledger::wallet::StoredKeypair;
use namada::ledger::wallet::{gen_sk, Store, ValidatorKeys};
#[cfg(not(feature = "dev"))]
use namada::types::address::Address;
use namada::types::key::*;
use namada::types::transaction::EllipticCurve;
use thiserror::Error;

use crate::config::genesis::genesis_config::GenesisConfig;
use crate::wallet::CliWalletUtils;

#[derive(Error, Debug)]
pub enum LoadStoreError {
    #[error("Failed decoding the wallet store: {0}")]
    Decode(toml::de::Error),
    #[error("Failed to read the wallet store from {0}: {1}")]
    ReadWallet(String, String),
    #[error("Failed to write the wallet store: {0}")]
    StoreNewWallet(String),
}

/// Wallet file name
const FILE_NAME: &str = "wallet.toml";

/// Get the path to the wallet store.
pub fn wallet_file(store_dir: impl AsRef<Path>) -> PathBuf {
    store_dir.as_ref().join(FILE_NAME)
}

/// Save the wallet store to a file.
pub fn save(store: &Store, store_dir: &Path) -> std::io::Result<()> {
    let data = store.encode();
    let wallet_path = wallet_file(store_dir);
    // Make sure the dir exists
    let wallet_dir = wallet_path.parent().unwrap();
    fs::create_dir_all(wallet_dir)?;
    // Write the file
    let options = FileOptions::new().create(true).write(true).truncate(true);
    let mut filelock =
        FileLock::lock(wallet_path.to_str().unwrap(), true, options)?;
    filelock.file.write_all(&data)
}

/// Load the store file or create a new one without any keys or addresses.
pub fn load_or_new(store_dir: &Path) -> Result<Store, LoadStoreError> {
    load(store_dir).or_else(|_| {
        let store = Store::default();
        save(&store, store_dir)
            .map_err(|err| LoadStoreError::StoreNewWallet(err.to_string()))?;
        Ok(store)
    })
}

/// Load the store file or create a new one with the default addresses from
/// the genesis file, if not found.
pub fn load_or_new_from_genesis(
    store_dir: &Path,
    genesis_cfg: GenesisConfig,
) -> Result<Store, LoadStoreError> {
    load(store_dir).or_else(|_| {
        #[cfg(not(feature = "dev"))]
        let store = new(genesis_cfg);
        #[cfg(feature = "dev")]
        let store = {
            // The function is unused in dev
            let _ = genesis_cfg;
            new()
        };
        save(&store, store_dir)
            .map_err(|err| LoadStoreError::StoreNewWallet(err.to_string()))?;
        Ok(store)
    })
}

/// Attempt to load the store file.
pub fn load(store_dir: &Path) -> Result<Store, LoadStoreError> {
    let wallet_file = wallet_file(store_dir);
    match FileLock::lock(
        wallet_file.to_str().unwrap(),
        true,
        FileOptions::new().read(true).write(false),
    ) {
        Ok(mut filelock) => {
            let mut store = Vec::<u8>::new();
            filelock.file.read_to_end(&mut store).map_err(|err| {
                LoadStoreError::ReadWallet(
                    store_dir.to_str().unwrap().parse().unwrap(),
                    err.to_string(),
                )
            })?;
            Store::decode(store).map_err(LoadStoreError::Decode)
        }
        Err(err) => Err(LoadStoreError::ReadWallet(
            wallet_file.to_string_lossy().into_owned(),
            err.to_string(),
        )),
    }
}

/// Add addresses from a genesis configuration.
#[cfg(not(feature = "dev"))]
pub fn add_genesis_addresses(store: &mut Store, genesis: GenesisConfig) {
    for (alias, addr) in
        super::defaults::addresses_from_genesis(genesis.clone())
    {
        store.insert_address::<CliWalletUtils>(alias, addr, true);
    }
    for (alias, token) in &genesis.token {
        if let Some(address) = token.address.as_ref() {
            match Address::from_str(address) {
                Ok(address) => {
                    store.add_vp_type_to_address(AddressVpType::Token, address)
                }
                Err(_) => {
                    tracing::error!(
                        "Weird address for token {alias}: {address}"
                    )
                }
            }
        }
    }
}

#[cfg(not(feature = "dev"))]
fn new(genesis: GenesisConfig) -> Store {
    let mut store = Store::default();
    add_genesis_addresses(&mut store, genesis);
    store
}

#[cfg(feature = "dev")]
fn new() -> Store {
    let mut store = Store::default();
    // Pre-load the default keys without encryption
    let no_password = None;
    for (alias, keypair) in super::defaults::keys() {
        let pkh: PublicKeyHash = (&keypair.ref_to()).into();
        store.insert_keypair::<CliWalletUtils>(
            alias,
            StoredKeypair::new(keypair, no_password.clone()).0,
            pkh,
            true,
        );
    }
    for (alias, addr) in super::defaults::addresses() {
        store.insert_address::<CliWalletUtils>(alias, addr, true);
    }
    store
}

/// Generate keypair for signing protocol txs and for the DKG
/// A protocol keypair may be optionally provided
///
/// Note that this removes the validator data.
pub fn gen_validator_keys(
    protocol_keypair: Option<common::SecretKey>,
    scheme: SchemeType,
) -> ValidatorKeys {
    let protocol_keypair = protocol_keypair.unwrap_or_else(|| gen_sk(scheme));
    let dkg_keypair = ferveo_common::Keypair::<EllipticCurve>::new(
        &mut StdRng::from_entropy(),
    );
    ValidatorKeys {
        protocol_keypair,
        dkg_keypair: Some(dkg_keypair.into()),
    }
}

#[cfg(all(test, feature = "dev"))]
mod test_wallet {
    use namada::types::address::Address;

    use super::*;

    #[test]
    fn test_toml_roundtrip_ed25519() {
        let mut store = new();
        let validator_keys = gen_validator_keys(None, SchemeType::Ed25519);
        store.add_validator_data(
            Address::decode("atest1v4ehgw36x3prswzxggunzv6pxqmnvdj9xvcyzvpsggeyvs3cg9qnywf589qnwvfsg5erg3fkl09rg5").unwrap(),
            validator_keys
        );
        let data = store.encode();
        let _ = Store::decode(data).expect("Test failed");
    }

    #[test]
    fn test_toml_roundtrip_secp256k1() {
        let mut store = new();
        let validator_keys = gen_validator_keys(None, SchemeType::Secp256k1);
        store.add_validator_data(
            Address::decode("atest1v4ehgw36x3prswzxggunzv6pxqmnvdj9xvcyzvpsggeyvs3cg9qnywf589qnwvfsg5erg3fkl09rg5").unwrap(),
            validator_keys
        );
        let data = store.encode();
        let _ = Store::decode(data).expect("Test failed");
    }
}
