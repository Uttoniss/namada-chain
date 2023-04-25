use std::collections::HashMap;
use std::fmt::Display;

use borsh::{BorshDeserialize, BorshSerialize};
use rust_decimal::Decimal;

use super::cli::offline::OfflineVote;
use super::storage::proposal::ProposalType;
use super::storage::vote::StorageProposalVote;
use crate::types::address::Address;
use crate::types::token::SCALE;

pub enum ProposalStatus {
    Pending,
    OnGoing,
    Ended,
}

impl Display for ProposalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposalStatus::Pending => write!(f, "pending"),
            ProposalStatus::OnGoing => write!(f, "on-going"),
            ProposalStatus::Ended => write!(f, "ended"),
        }
    }
}

pub type VotePower = u128;

#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct Vote {
    pub validator: Address,
    pub delegator: Address,
    pub data: StorageProposalVote,
}

impl Vote {
    pub fn is_validator(&self) -> bool {
        self.validator.eq(&self.delegator)
    }
}

pub enum TallyType {
    TwoThird,
    OneThird,
    LessOneThirdNay,
}

impl From<ProposalType> for TallyType {
    fn from(proposal_type: ProposalType) -> Self {
        match proposal_type {
            ProposalType::Default(_) => TallyType::TwoThird,
            ProposalType::PGFSteward(_) => TallyType::OneThird,
            ProposalType::PGFPayment(_) => TallyType::LessOneThirdNay,
            ProposalType::ETHBridge(_) => TallyType::TwoThird,
        }
    }
}

/// The result of a proposal
pub enum TallyResult {
    /// Proposal was accepted with the associated value
    Passed,
    /// Proposal was rejected
    Rejected,
}

impl Display for TallyResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TallyResult::Passed => write!(f, "passed"),
            TallyResult::Rejected => write!(f, "rejected"),
        }
    }
}

impl TallyResult {
    pub fn new(
        tally_type: &TallyType,
        yay_voting_power: VotePower,
        nay_voting_power: VotePower,
        total_voting_power: VotePower,
    ) -> Self {
        let passed = match tally_type {
            TallyType::TwoThird => {
                yay_voting_power >= (total_voting_power / 3) * 2
            }
            TallyType::OneThird => yay_voting_power >= total_voting_power / 3,
            TallyType::LessOneThirdNay => {
                nay_voting_power <= total_voting_power / 3
            }
        };

        if passed { Self::Passed } else { Self::Rejected }
    }
}

/// The result with votes of a proposal
pub struct ProposalResult {
    /// The result of a proposal
    pub result: TallyResult,
    /// The total voting power during the proposal tally
    pub total_voting_power: VotePower,
    /// The total voting power from yay votes
    pub total_yay_power: VotePower,
    /// The total voting power from nay votes (unused at the moment)
    pub total_nay_power: VotePower,
}

impl Display for ProposalResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let percentage = Decimal::checked_div(
            self.total_yay_power.into(),
            self.total_voting_power.into(),
        )
        .unwrap_or_default();

        write!(
            f,
            "{} with {} yay votes and {} nay votes ({:.2}%)",
            self.result,
            self.total_yay_power / SCALE as u128,
            self.total_nay_power / SCALE as u128,
            percentage.checked_mul(100.into()).unwrap_or_default()
        )
    }
}

pub enum TallyVote {
    OnChain(StorageProposalVote),
    Offline(OfflineVote),
}

impl From<StorageProposalVote> for TallyVote {
    fn from(vote: StorageProposalVote) -> Self {
        Self::OnChain(vote)
    }
}

impl From<OfflineVote> for TallyVote {
    fn from(vote: OfflineVote) -> Self {
        Self::Offline(vote)
    }
}

impl TallyVote {
    pub fn is_yay(&self) -> bool {
        match self {
            TallyVote::OnChain(vote) => vote.is_yay(),
            TallyVote::Offline(vote) => vote.is_yay(),
        }
    }

    pub fn is_same_side(&self, other: &TallyVote) -> bool {
        let both_yay = self.is_yay() && other.is_yay();
        let both_nay = !self.is_yay() && !other.is_yay();

        both_yay || !both_nay
    }
}

/// Proposal structure holding votes information necessary to compute the
/// outcome
pub struct ProposalVotes {
    /// Map from validator address to vote
    pub validators_vote: HashMap<Address, TallyVote>,
    /// Map from validator to their voting power
    pub validator_voting_power: HashMap<Address, VotePower>,
    /// Map from delegation address to their vote
    pub delegators_vote: HashMap<Address, TallyVote>,
    /// Map from delegator address to the corresponding validator voting power
    pub delegator_voting_power: HashMap<Address, HashMap<Address, VotePower>>,
}

pub fn compute_proposal_result(
    votes: ProposalVotes,
    total_voting_power: VotePower,
    tally_at: TallyType,
) -> ProposalResult {
    let mut yay_voting_power = VotePower::default();
    let mut nay_voting_power = VotePower::default();

    for (address, vote_power) in votes.validator_voting_power {
        let vote_type = votes.validators_vote.get(&address);
        if let Some(vote) = vote_type {
            if vote.is_yay() {
                yay_voting_power += vote_power;
            } else {
                nay_voting_power += vote_power;
            }
        }
    }

    for (delegator, degalations) in votes.delegator_voting_power {
        let delegator_vote = match votes.delegators_vote.get(&delegator) {
            Some(vote) => vote,
            None => continue,
        };
        for (validator, voting_power) in degalations {
            let validator_vote = votes.validators_vote.get(&validator);
            if let Some(validator_vote) = validator_vote {
                if !validator_vote.is_same_side(delegator_vote) {
                    if delegator_vote.is_yay() {
                        yay_voting_power += voting_power;
                        nay_voting_power -= voting_power;
                    } else {
                        nay_voting_power += voting_power;
                        yay_voting_power -= voting_power;
                    }
                }
            } else if delegator_vote.is_yay() {
                yay_voting_power += voting_power;
            } else {
                nay_voting_power += voting_power;
            }
        }
    }

    let tally_result = TallyResult::new(
        &tally_at,
        yay_voting_power,
        nay_voting_power,
        total_voting_power,
    );

    ProposalResult {
        result: tally_result,
        total_voting_power,
        total_yay_power: yay_voting_power,
        total_nay_power: nay_voting_power,
    }
}
