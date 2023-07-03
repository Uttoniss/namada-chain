use std::fmt::Display;

use borsh::{BorshDeserialize, BorshSerialize};

use super::storage as gov_storage;
use crate::ledger::storage_api::{self, StorageRead, StorageWrite};
use crate::types::token::Amount;

#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    BorshSerialize,
    BorshDeserialize,
)]
/// Governance parameter structure
pub struct GovParams {
    /// Minimum amount of locked funds
    pub min_proposal_fund: u64,
    /// Maximum kibibyte length for proposal code
    pub max_proposal_code_size: u64,
    /// Minimum proposal voting period in epochs
    pub min_proposal_period: u64,
    /// Maximum proposal voting period in epochs
    pub max_proposal_period: u64,
    /// Maximum number of characters for proposal content
    pub max_proposal_content_size: u64,
    /// Minimum epochs between end and grace epochs
    pub min_proposal_grace_epochs: u64,
}

impl Display for GovParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Min. proposal fund: {}\nMax. proposal code size: {}\nMin. \
             proposal period: {}\nMax. proposal period: {}\nMax. proposal \
             content size: {}\nMin. proposal grace epochs: {}",
            self.min_proposal_fund,
            self.max_proposal_code_size,
            self.min_proposal_period,
            self.max_proposal_period,
            self.max_proposal_content_size,
            self.min_proposal_grace_epochs
        )
    }
}

impl Default for GovParams {
    fn default() -> Self {
        Self {
            min_proposal_fund: 500,
            max_proposal_code_size: 300_000,
            min_proposal_period: 3,
            max_proposal_period: 27,
            max_proposal_content_size: 10_000,
            min_proposal_grace_epochs: 6,
        }
    }
}

impl GovParams {
    /// Initialize governance parameters into storage
    pub fn init_storage<S>(&self, storage: &mut S) -> storage_api::Result<()>
    where
        S: StorageRead + StorageWrite,
    {
        let Self {
            min_proposal_fund,
            max_proposal_code_size,
            min_proposal_period,
            max_proposal_period,
            max_proposal_content_size,
            min_proposal_grace_epochs,
        } = self;

        let min_proposal_fund_key = gov_storage::get_min_proposal_fund_key();
        let amount = Amount::native_whole(*min_proposal_fund);
        storage.write(&min_proposal_fund_key, amount)?;

        let max_proposal_code_size_key =
            gov_storage::get_max_proposal_code_size_key();
        storage.write(&max_proposal_code_size_key, max_proposal_code_size)?;

        let min_proposal_period_key =
            gov_storage::get_min_proposal_period_key();
        storage.write(&min_proposal_period_key, min_proposal_period)?;

        let max_proposal_period_key =
            gov_storage::get_max_proposal_period_key();
        storage.write(&max_proposal_period_key, max_proposal_period)?;

        let max_proposal_content_size_key =
            gov_storage::get_max_proposal_content_key();
        storage
            .write(&max_proposal_content_size_key, max_proposal_content_size)?;

        let min_proposal_grace_epoch_key =
            gov_storage::get_min_proposal_grace_epoch_key();
        storage
            .write(&min_proposal_grace_epoch_key, min_proposal_grace_epochs)?;

        let counter_key = gov_storage::get_counter_key();
        storage.write(&counter_key, u64::MIN)
    }
}
