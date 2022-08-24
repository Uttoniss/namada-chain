//! Contains types necessary for processing validator set updates
//! in vote extensions.

pub mod encoding;

use std::collections::HashMap;

use borsh::{BorshDeserialize, BorshSchema, BorshSerialize};
use encoding::{AbiEncode, Encode, Token};
use ethabi::ethereum_types as ethereum;
use num_rational::Ratio;

use crate::ledger::pos::types::{Epoch, VotingPower};
use crate::proto::Signed;
use crate::types::address::Address;
use crate::types::ethereum_events::{EthAddress, KeccakHash};
use crate::types::key::common::{self, Signature};

// the namespace strings plugged into validator set hashes
const BRIDGE_CONTRACT_NAMESPACE: &str = "bridge";
const GOVERNANCE_CONTRACT_NAMESPACE: &str = "governance";

/// Contains the digest of all signatures from a quorum of
/// validators for a [`Vext`].
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize, BorshSchema)]
pub struct VextDigest {
    /// A mapping from a validator address to a [`Signature`].
    pub signatures: HashMap<Address, Signature>,
    /// The addresses of the validators in the new [`Epoch`],
    /// and their respective voting power.
    pub voting_powers: VotingPowersMap,
}

impl VextDigest {
    /// Decompresses a set of signed [`Vext`] instances.
    pub fn decompress(self, epoch: Epoch) -> Vec<SignedVext> {
        let VextDigest {
            signatures,
            voting_powers,
        } = self;

        let mut extensions = vec![];

        for (validator_addr, signature) in signatures.into_iter() {
            let voting_powers = voting_powers.clone();
            let data = Vext {
                validator_addr,
                voting_powers,
                epoch,
            };
            extensions.push(SignedVext::new_from(data, signature));
        }
        extensions
    }

    /// Returns an Ethereum ABI encoded string with the
    /// params to feed to the Ethereum bridge smart contracts.
    pub fn abi_params(&self) -> String {
        todo!()
    }
}

/// Represents a [`Vext`] signed by some validator, with
/// an Ethereum key.
pub type SignedVext = Signed<Vext, SerializeWithAbiEncode>;

/// Represents a validator set update, for some new [`Epoch`].
#[derive(
    Eq, PartialEq, Clone, Debug, BorshSerialize, BorshDeserialize, BorshSchema,
)]
pub struct Vext {
    /// The addresses of the validators in the new [`Epoch`],
    /// and their respective voting power.
    ///
    /// When signing a [`Vext`], this [`VotingPowersMap`] is converted
    /// into two arrays: one for its keys, and another for its
    /// values. The arrays are sorted in descending order based
    /// on the voting power of each validator.
    pub voting_powers: VotingPowersMap,
    /// TODO: the validator's address is temporarily being included
    /// until we're able to map a Tendermint address to a validator
    /// address (see <https://github.com/anoma/namada/issues/200>)
    pub validator_addr: Address,
    /// The new [`Epoch`].
    ///
    /// Since this is a monotonically growing sequence number,
    /// it is signed together with the rest of the data to
    /// prevent replay attacks on validator set updates.
    ///
    /// Additionally, we can use this [`Epoch`] value to query the appropriate
    /// validator set to verify signatures with.
    pub epoch: Epoch,
}

impl Vext {
    /// Creates a new signed [`Vext`].
    ///
    /// For more information, read the docs of [`SignedVext::new`].
    #[inline]
    pub fn sign(&self, sk: &common::SecretKey) -> SignedVext {
        SignedVext::new(sk, self.clone())
    }
}

/// Provides a mapping between [`EthAddress`] and [`VotingPower`] instances.
pub type VotingPowersMap = HashMap<EthAddress, VotingPower>;

/// This trait contains additional methods for a [`HashMap`], related
/// with validator set update vote extensions logic.
pub trait VotingPowersMapExt {
    /// Returns the keccak hash of this [`VotingPowersMap`]
    /// to be signed by an Ethereum validator key.
    fn get_bridge_hash(&self, epoch: Epoch) -> KeccakHash;

    /// Returns the keccak hash of this [`VotingPowersMap`]
    /// to be signed by an Ethereum governance key.
    fn get_governance_hash(&self, epoch: Epoch) -> KeccakHash;

    /// Returns the list of Ethereum validator addresses and their respective
    /// voting power (in this order), with an Ethereum ABI compatible encoding.
    fn get_abi_encoded(&self) -> (Vec<Token>, Vec<Token>);
}

impl VotingPowersMapExt for VotingPowersMap {
    #[inline]
    fn get_bridge_hash(&self, epoch: Epoch) -> KeccakHash {
        let (validators, voting_powers) = self.get_abi_encoded();

        compute_hash(
            epoch,
            BRIDGE_CONTRACT_NAMESPACE,
            validators,
            voting_powers,
        )
    }

    #[inline]
    fn get_governance_hash(&self, epoch: Epoch) -> KeccakHash {
        compute_hash(
            epoch,
            GOVERNANCE_CONTRACT_NAMESPACE,
            // TODO: get governance validators
            vec![],
            // TODO: get governance voting powers
            vec![],
        )
    }

    fn get_abi_encoded(&self) -> (Vec<Token>, Vec<Token>) {
        // get addresses and voting powers all into one vec
        let mut unsorted: Vec<_> = self.iter().collect();

        // sort it by voting power, in descending order
        unsorted.sort_by(|&(_, ref power_1), &(_, ref power_2)| {
            power_2.cmp(power_1)
        });

        let sorted = unsorted;
        let total_voting_power: u64 = sorted
            .iter()
            .map(|&(_, &voting_power)| u64::from(voting_power))
            .sum();

        // split the vec into two
        sorted
            .into_iter()
            .map(|(&EthAddress(addr), &voting_power)| {
                let voting_power: u64 = voting_power.into();

                // normalize the voting power
                // https://github.com/anoma/ethereum-bridge/blob/main/test/utils/utilities.js#L29
                const NORMALIZED_VOTING_POWER: u64 = 1 << 32;

                let voting_power = Ratio::new(voting_power, total_voting_power)
                    * NORMALIZED_VOTING_POWER;
                let voting_power = voting_power.round().to_integer();
                let voting_power: ethereum::U256 = voting_power.into();

                (
                    Token::Address(ethereum::H160(addr)),
                    Token::Uint(voting_power),
                )
            })
            .unzip()
    }
}

/// Convert an [`Epoch`] to a [`Token`].
#[inline]
fn epoch_to_token(epoch: Epoch) -> Token {
    Token::Uint(u64::from(epoch).into())
}

/// Compute the keccak hash of a validator set update.
///
/// For more information, check the Ethereum bridge smart contracts:
//    - <https://github.com/anoma/ethereum-bridge/blob/main/contracts/contract/Governance.sol#L232>
//    - <https://github.com/anoma/ethereum-bridge/blob/main/contracts/contract/Bridge.sol#L201>
#[inline]
fn compute_hash(
    epoch: Epoch,
    namespace: &str,
    validators: Vec<Token>,
    voting_powers: Vec<Token>,
) -> KeccakHash {
    AbiEncode::keccak256(&[
        Token::String(namespace.into()),
        Token::Array(validators),
        Token::Array(voting_powers),
        epoch_to_token(epoch),
    ])
}

// this is only here so we don't pollute the
// outer namespace with serde traits
mod tag {
    use serde::{Deserialize, Serialize};

    use super::encoding::{AbiEncode, Encode, Token};
    use super::{epoch_to_token, Vext, VotingPowersMapExt};
    use crate::proto::SignedSerialize;
    use crate::types::ethereum_events::KeccakHash;

    /// Tag type that indicates we should use [`AbiEncode`]
    /// to sign data in a [`crate::proto::Signed`] wrapper.
    #[derive(Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
    pub struct SerializeWithAbiEncode;

    impl SignedSerialize<Vext> for SerializeWithAbiEncode {
        type Output = [u8; 32];

        fn serialize(ext: &Vext) -> Self::Output {
            let KeccakHash(output) = AbiEncode::signed_keccak256(&[
                Token::String("updateValidatorsSet".into()),
                Token::FixedBytes(
                    ext.voting_powers.get_bridge_hash(ext.epoch).0.to_vec(),
                ),
                Token::FixedBytes(
                    ext.voting_powers.get_governance_hash(ext.epoch).0.to_vec(),
                ),
                epoch_to_token(ext.epoch),
            ]);
            output
        }
    }
}

#[doc(inline)]
pub use tag::SerializeWithAbiEncode;
