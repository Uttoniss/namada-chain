pub mod defaults;
mod keys;
pub mod pre_genesis;
mod store;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{env, fs};

pub use namada::ledger::wallet::alias::Alias;
use namada::ledger::wallet::{
    ConfirmationResponse, FindKeyError, Wallet, WalletUtils,
};
pub use namada::ledger::wallet::{
    DecryptionError, StoredKeypair, ValidatorData, ValidatorKeys,
};
use namada::types::key::*;
pub use store::wallet_file;

use crate::cli;
use crate::config::genesis::genesis_config::GenesisConfig;

#[derive(Debug)]
pub struct CliWalletUtils;

impl WalletUtils for CliWalletUtils {
    type Storage = PathBuf;

    /// Read the password for encryption/decryption from the file/env/stdin.
    /// Panics if all options are empty/invalid.
    fn read_password(prompt_msg: &str) -> String {
        let pwd = match env::var("NAMADA_WALLET_PASSWORD_FILE") {
            Ok(path) => fs::read_to_string(path)
                .expect("Something went wrong reading the file"),
            Err(_) => match env::var("NAMADA_WALLET_PASSWORD") {
                Ok(password) => password,
                Err(_) => rpassword::read_password_from_tty(Some(prompt_msg))
                    .unwrap_or_default(),
            },
        };
        if pwd.is_empty() {
            eprintln!("Password cannot be empty");
            cli::safe_exit(1)
        }
        pwd
    }

    /// Read an alias from the file/env/stdin.
    fn read_alias(prompt_msg: &str) -> String {
        print!("Choose an alias for {}: ", prompt_msg);
        io::stdout().flush().unwrap();
        let mut alias = String::new();
        io::stdin().read_line(&mut alias).unwrap();
        alias.trim().to_owned()
    }

    // The given alias has been selected but conflicts with another alias in
    // the store. Offer the user to either replace existing mapping, alter the
    // chosen alias to a name of their chosing, or cancel the aliasing.
    fn show_overwrite_confirmation(
        alias: &Alias,
        alias_for: &str,
    ) -> ConfirmationResponse {
        print!(
            "You're trying to create an alias \"{}\" that already exists for \
             {} in your store.\nWould you like to replace it? \
             s(k)ip/re(p)lace/re(s)elect: ",
            alias, alias_for
        );
        io::stdout().flush().unwrap();

        let mut buffer = String::new();
        // Get the user to select between 3 choices
        match io::stdin().read_line(&mut buffer) {
            Ok(size) if size > 0 => {
                // Isolate the single character representing the choice
                let byte = buffer.chars().next().unwrap();
                buffer.clear();
                match byte {
                    'p' | 'P' => return ConfirmationResponse::Replace,
                    's' | 'S' => {
                        // In the case of reselection, elicit new alias
                        print!("Please enter a different alias: ");
                        io::stdout().flush().unwrap();
                        if io::stdin().read_line(&mut buffer).is_ok() {
                            return ConfirmationResponse::Reselect(
                                buffer.trim().into(),
                            );
                        }
                    }
                    'k' | 'K' => return ConfirmationResponse::Skip,
                    // Input is senseless fall through to repeat prompt
                    _ => {}
                };
            }
            _ => {}
        }
        // Input is senseless fall through to repeat prompt
        println!("Invalid option, try again.");
        Self::show_overwrite_confirmation(alias, alias_for)
    }
}

/// Generate keypair
/// for signing protocol txs and for the DKG (which will also be stored)
/// A protocol keypair may be optionally provided, indicating that
/// we should re-use a keypair already in the wallet
pub fn gen_validator_keys<U: WalletUtils>(
    wallet: &mut Wallet<U>,
    protocol_pk: Option<common::PublicKey>,
    scheme: SchemeType,
) -> Result<ValidatorKeys, FindKeyError> {
    let protocol_keypair = protocol_pk.map(|pk| {
        wallet
            .find_key_by_pkh(&PublicKeyHash::from(&pk), None)
            .ok()
            .or_else(|| {
                wallet
                    .store_mut()
                    .validator_data()
                    .take()
                    .map(|data| data.keys.protocol_keypair.clone())
            })
            .ok_or(FindKeyError::KeyNotFound)
    });
    match protocol_keypair {
        Some(Err(err)) => Err(err),
        other => Ok(store::gen_validator_keys(
            other.map(|res| res.unwrap()),
            scheme,
        )),
    }
}

/// Add addresses from a genesis configuration.
pub fn add_genesis_addresses(
    wallet: &mut Wallet<CliWalletUtils>,
    genesis: GenesisConfig,
) {
    for (alias, addr) in defaults::addresses_from_genesis(genesis) {
        wallet.add_address(alias.normalize(), addr, true);
    }
}

/// Save the wallet store to a file.
pub fn save(wallet: &Wallet<CliWalletUtils>) -> std::io::Result<()> {
    self::store::save(wallet.store(), wallet.store_dir())
}

/// Load a wallet from the store file.
pub fn load(store_dir: &Path) -> Option<Wallet<CliWalletUtils>> {
    let store = self::store::load(store_dir).unwrap_or_else(|err| {
        eprintln!("Unable to load the wallet: {}", err);
        cli::safe_exit(1)
    });
    Some(Wallet::<CliWalletUtils>::new(
        store_dir.to_path_buf(),
        store,
    ))
}

/// Load a wallet from the store file or create a new wallet without any
/// keys or addresses.
pub fn load_or_new(store_dir: &Path) -> Wallet<CliWalletUtils> {
    let store = self::store::load_or_new(store_dir).unwrap_or_else(|err| {
        eprintln!("Unable to load the wallet: {}", err);
        cli::safe_exit(1)
    });
    Wallet::<CliWalletUtils>::new(store_dir.to_path_buf(), store)
}

/// Load a wallet from the store file or create a new one with the default
/// addresses loaded from the genesis file, if not found.
pub fn load_or_new_from_genesis(
    store_dir: &Path,
    genesis_cfg: GenesisConfig,
) -> Wallet<CliWalletUtils> {
    let store = self::store::load_or_new_from_genesis(store_dir, genesis_cfg)
        .unwrap_or_else(|err| {
            eprintln!("Unable to load the wallet: {}", err);
            cli::safe_exit(1)
        });
    Wallet::<CliWalletUtils>::new(store_dir.to_path_buf(), store)
}

/// Read the password for encryption from the file/env/stdin with
/// confirmation.
pub fn read_and_confirm_pwd(unsafe_dont_encrypt: bool) -> Option<String> {
    let password = if unsafe_dont_encrypt {
        println!("Warning: The keypair will NOT be encrypted.");
        None
    } else {
        Some(CliWalletUtils::read_password(
            "Enter your encryption password: ",
        ))
    };
    // Bis repetita for confirmation.
    let to_confirm = if unsafe_dont_encrypt {
        None
    } else {
        Some(CliWalletUtils::read_password(
            "To confirm, please enter the same encryption password once more: ",
        ))
    };
    if to_confirm != password {
        eprintln!("Your two inputs do not match!");
        cli::safe_exit(1)
    }
    password
}
