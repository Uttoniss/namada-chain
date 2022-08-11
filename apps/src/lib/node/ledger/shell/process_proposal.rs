//! Implementation of the ['VerifyHeader`], [`ProcessProposal`],
//! and [`RevertProposal`] ABCI++ methods for the Shell
use namada::types::transaction::protocol::ProtocolTxType;
use namada::types::voting_power::FractionalVotingPower;
#[cfg(not(feature = "ABCI"))]
use tendermint_proto::abci::response_process_proposal::ProposalStatus;
#[cfg(not(feature = "ABCI"))]
use tendermint_proto::abci::{
    ExecTxResult, RequestProcessProposal, ResponseProcessProposal,
};
#[cfg(feature = "ABCI")]
use tendermint_proto_abci::abci::RequestDeliverTx;

use super::queries::QueriesExt;
use super::*;

impl<D, H> Shell<D, H>
where
    D: DB + for<'iter> DBIter<'iter> + Sync + 'static,
    H: StorageHasher + Sync + 'static,
{
    /// INVARIANT: This method must be stateless.
    pub fn verify_header(
        &self,
        _req: shim::request::VerifyHeader,
    ) -> shim::response::VerifyHeader {
        Default::default()
    }

    /// Check all the txs in a block. Some txs may be incorrect,
    /// but we only reject the entire block if the order of the
    /// included txs violates the order decided upon in the previous
    /// block.
    #[cfg(not(feature = "ABCI"))]
    pub fn process_proposal(
        &mut self,
        req: RequestProcessProposal,
    ) -> ResponseProcessProposal {
        tracing::info!(
            proposer = ?hex::encode(&req.proposer_address),
            height = req.height,
            hash = ?hex::encode(&req.hash),
            n_txs = req.txs.len(),
            "Received block proposal",
        );
        // the number of vote extension digests included in the block proposal
        let mut vote_ext_digest_num = 0;
        let tx_results: Vec<ExecTxResult> = req
            .txs
            .iter()
            .map(|tx_bytes| {
                ExecTxResult::from(
                    self.process_single_tx(tx_bytes, &mut vote_ext_digest_num),
                )
            })
            .collect();

        // We should not have more than one `ethereum_events::VextDigest` in
        // a proposal from some round's leader.
        let too_many_vext_digests = vote_ext_digest_num > 1;
        if too_many_vext_digests {
            tracing::warn!(
                proposer = ?hex::encode(&req.proposer_address),
                height = req.height,
                hash = ?hex::encode(&req.hash),
                vote_ext_digest_num,
                "Found too many vote extension transactions, proposed block \
                 will be rejected"
            );
        }

        // Erroneous transactions were detected when processing
        // the leader's proposal. We allow txs that do not
        // deserialize properly, that have invalid signatures
        // and that have invalid wasm code to reach FinalizeBlock.
        let invalid_txs = tx_results.iter().any(|res| {
            let error = ErrorCodes::from_u32(res.code).expect(
                "All error codes returned from process_single_tx are valid",
            );
            !error.is_recoverable()
        });
        if invalid_txs {
            tracing::warn!(
                proposer = ?hex::encode(&req.proposer_address),
                height = req.height,
                hash = ?hex::encode(&req.hash),
                "Found invalid transactions, proposed block will be rejected"
            );
        }

        let status = if too_many_vext_digests || invalid_txs {
            ProposalStatus::Reject
        } else {
            ProposalStatus::Accept
        };
        tracing::info!(
            proposer = ?hex::encode(&req.proposer_address),
            height = req.height,
            hash = ?hex::encode(&req.hash),
            ?status,
            "Processed block proposal",
        );
        ResponseProcessProposal {
            status: status as i32,
            tx_results,
            ..Default::default()
        }
    }

    /// Checks if the Tx can be deserialized from bytes. Checks the fees and
    /// signatures of the fee payer for a transaction if it is a wrapper tx.
    ///
    /// Checks validity of a decrypted tx or that a tx marked un-decryptable
    /// is in fact so. Also checks that decrypted txs were submitted in
    /// correct order.
    ///
    /// Error codes:
    ///   0: Ok
    ///   1: Invalid tx
    ///   2: Tx is invalidly signed
    ///   3: Wasm runtime error
    ///   4: Invalid order of decrypted txs
    ///   5. More decrypted txs than expected
    ///   6. A transaction could not be decrypted
    ///   7. An error in the vote extensions included in the proposal
    ///
    /// INVARIANT: Any changes applied in this method must be reverted if the
    /// proposal is rejected (unless we can simply overwrite them in the
    /// next block).
    pub(crate) fn process_single_tx(
        &mut self,
        tx_bytes: &[u8],
        vote_ext_digest_num: &mut usize,
    ) -> TxResult {
        let maybe_tx = Tx::try_from(tx_bytes).map_or_else(
            |err| {
                tracing::debug!(
                    ?err,
                    "Couldn't deserialize transaction received during \
                     PrepareProposal"
                );
                Err(TxResult {
                    code: ErrorCodes::InvalidTx.into(),
                    info: "The submitted transaction was not deserializable"
                        .into(),
                })
            },
            |tx| {
                process_tx(tx).map_err(|err| {
                    // This occurs if the wrapper / protocol tx signature is
                    // invalid
                    TxResult {
                        code: ErrorCodes::InvalidSig.into(),
                        info: err.to_string(),
                    }
                })
            },
        );
        let tx = match maybe_tx {
            Ok(tx) => tx,
            Err(tx_result) => return tx_result,
        };

        // TODO: This should not be hardcoded
        let privkey = <EllipticCurve as PairingEngine>::G2Affine::prime_subgroup_generator();

        match tx {
            // If it is a raw transaction, we do no further validation
            TxType::Raw(_) => TxResult {
                code: ErrorCodes::InvalidTx.into(),
                info: "Transaction rejected: Non-encrypted transactions are \
                       not supported"
                    .into(),
            },
            TxType::Protocol(protocol_tx) => match protocol_tx.tx {
                ProtocolTxType::EthereumEvents(digest) => {
                    *vote_ext_digest_num += 1;

                    let extensions =
                        digest.decompress(self.storage.last_height);
                    let valid_extensions =
                        self.validate_vote_extension_list(extensions);

                    let mut voting_power = FractionalVotingPower::default();
                    let total_power = {
                        let epoch =
                            self.storage.block.pred_epochs.get_epoch(
                                BlockHeight(self.storage.last_height.0),
                            );
                        u64::from(self.storage.get_total_voting_power(epoch))
                    };

                    if valid_extensions.into_iter().all(|maybe_ext| {
                        maybe_ext
                            .map(|(power, _)| {
                                voting_power += FractionalVotingPower::new(
                                    u64::from(power),
                                    total_power,
                                )
                                .expect(
                                    "The voting power we obtain from storage \
                                     should always be valid",
                                );
                            })
                            .is_ok()
                    }) {
                        if voting_power > FractionalVotingPower::TWO_THIRDS {
                            TxResult {
                                code: ErrorCodes::Ok.into(),
                                info: "Process proposal accepted this \
                                       transaction"
                                    .into(),
                            }
                        } else {
                            TxResult {
                                code: ErrorCodes::InvalidVoteExtension.into(),
                                info: "Process proposal rejected this \
                                       proposal because the backing stake of \
                                       the vote extensions published in the \
                                       proposal was insufficient"
                                    .into(),
                            }
                        }
                    } else {
                        // TODO: maybe return a summary of the reasons for
                        // dropping a vote extension. we have access to the
                        // motives in `filtered_extensions`
                        TxResult {
                            code: ErrorCodes::InvalidVoteExtension.into(),
                            info: "Process proposal rejected this proposal \
                                   because at least one of the vote \
                                   extensions included was invalid."
                                .into(),
                        }
                    }
                }
                _ => TxResult {
                    code: ErrorCodes::InvalidTx.into(),
                    info: "Unsupported protocol transaction type".into(),
                },
            },
            TxType::Decrypted(tx) => match self.next_wrapper() {
                Some(wrapper) => {
                    if wrapper.tx_hash != tx.hash_commitment() {
                        TxResult {
                            code: ErrorCodes::InvalidOrder.into(),
                            info: "Process proposal rejected a decrypted \
                                   transaction that violated the tx order \
                                   determined in the previous block"
                                .into(),
                        }
                    } else if verify_decrypted_correctly(&tx, privkey) {
                        TxResult {
                            code: ErrorCodes::Ok.into(),
                            info: "Process Proposal accepted this transaction"
                                .into(),
                        }
                    } else {
                        TxResult {
                            code: ErrorCodes::InvalidTx.into(),
                            info: "The encrypted payload of tx was \
                                   incorrectly marked as un-decryptable"
                                .into(),
                        }
                    }
                }
                None => TxResult {
                    code: ErrorCodes::ExtraTxs.into(),
                    info: "Received more decrypted txs than expected".into(),
                },
            },
            TxType::Wrapper(wrapper) => {
                // validate the ciphertext via Ferveo
                if !wrapper.validate_ciphertext() {
                    TxResult {
                        code: ErrorCodes::InvalidTx.into(),
                        info: format!(
                            "The ciphertext of the wrapped tx {} is invalid",
                            hash_tx(tx_bytes)
                        ),
                    }
                } else {
                    // check that the fee payer has sufficient balance
                    let balance = self
                        .storage
                        .get_balance(&wrapper.fee.token, &wrapper.fee_payer())
                        .unwrap_or_default();

                    if wrapper.fee.amount <= balance {
                        TxResult {
                            code: ErrorCodes::Ok.into(),
                            info: "Process proposal accepted this transaction"
                                .into(),
                        }
                    } else {
                        TxResult {
                            code: ErrorCodes::InvalidTx.into(),
                            info: "The address given does not have sufficient \
                                   balance to pay fee"
                                .into(),
                        }
                    }
                }
            }
        }
    }

    /// If we are not using ABCI++, we check the wrapper,
    /// decode it, and check the decoded payload all at once
    #[cfg(feature = "ABCI")]
    pub fn process_and_decode_proposal(
        &mut self,
        req: RequestDeliverTx,
    ) -> shim::request::ProcessedTx {
        // check the wrapper tx
        let req_tx = match Tx::try_from(req.tx.as_ref()) {
            Ok(tx) => tx,
            Err(_) => {
                return shim::request::ProcessedTx {
                    result: TxResult {
                        code: ErrorCodes::InvalidTx.into(),
                        info: "The submitted transaction was not \
                               deserializable"
                            .into(),
                    },
                    // this ensures that emitted events are of the correct type
                    tx: req.tx,
                };
            }
        };
        match process_tx(req_tx.clone()) {
            Ok(TxType::Wrapper(_)) => {}
            Ok(TxType::Protocol(_)) => {
                let result = self.process_single_tx(&req.tx, &mut 0usize);
                return shim::request::ProcessedTx { tx: req.tx, result };
            }
            Ok(_) => {
                return shim::request::ProcessedTx {
                    result: TxResult {
                        code: ErrorCodes::InvalidTx.into(),
                        info: "Transaction rejected: Non-encrypted \
                               transactions are not supported"
                            .into(),
                    },
                    // this ensures that emitted events are of the correct type
                    tx: req.tx,
                };
            }
            Err(_) => {
                // This will be caught later
            }
        }

        let wrapper_resp = self.process_single_tx(&req.tx, &mut 0usize);
        let privkey = <EllipticCurve as PairingEngine>::G2Affine::prime_subgroup_generator();

        if wrapper_resp.code == 0 {
            // if the wrapper passed, decode it
            if let Ok(TxType::Wrapper(wrapper)) = process_tx(req_tx) {
                let decoded = Tx::from(match wrapper.decrypt(privkey) {
                    Ok(tx) => DecryptedTx::Decrypted(tx),
                    _ => DecryptedTx::Undecryptable(wrapper.clone()),
                })
                .to_bytes();
                // we are not checking that txs are out of order
                self.storage.tx_queue.push(wrapper);
                // check the decoded tx
                let decoded_resp =
                    self.process_single_tx(&decoded, &mut 0usize);
                // this ensures that the tx queue is empty even if an error
                // happened in [`process_proposal`].
                self.storage.tx_queue.pop();
                shim::request::ProcessedTx {
                    // this ensures that emitted events are of the correct type
                    tx: decoded,
                    result: decoded_resp,
                }
            } else {
                // This was checked above
                unreachable!()
            }
        } else {
            shim::request::ProcessedTx {
                // this ensures that emitted events are of the correct type
                tx: req.tx,
                result: wrapper_resp,
            }
        }
    }

    #[cfg(not(feature = "ABCI"))]
    pub fn revert_proposal(
        &mut self,
        _req: shim::request::RevertProposal,
    ) -> shim::response::RevertProposal {
        Default::default()
    }
}

/// We test the failure cases of [`process_proposal`]. The happy flows
/// are covered by the e2e tests.
#[cfg(test)]
mod test_process_proposal {
    use std::collections::{HashMap, HashSet};

    use assert_matches::assert_matches;
    use borsh::BorshDeserialize;
    use namada::proto::SignedTxData;
    use namada::types::address::xan;
    use namada::types::ethereum_events::EthereumEvent;
    use namada::types::hash::Hash;
    use namada::types::key::*;
    use namada::types::storage::Epoch;
    use namada::types::token::Amount;
    use namada::types::transaction::encrypted::EncryptedTx;
    use namada::types::transaction::{EncryptionKey, Fee};
    use namada::types::vote_extensions::ethereum_events::{
        self, MultiSignedEthEvent,
    };
    #[cfg(not(feature = "ABCI"))]
    use tendermint_proto::abci::RequestInitChain;
    #[cfg(not(feature = "ABCI"))]
    use tendermint_proto::google::protobuf::Timestamp;
    #[cfg(feature = "ABCI")]
    use tendermint_proto_abci::abci::RequestInitChain;
    #[cfg(feature = "ABCI")]
    use tendermint_proto_abci::google::protobuf::Timestamp;

    use super::*;
    use crate::node::ledger::shell::test_utils::{
        self, gen_keypair, ProcessProposal, TestError, TestShell,
    };
    use crate::wallet;

    /// Test that if a proposal contains more than one
    /// `ethereum_events::VextDigest`, we reject it.
    #[test]
    #[cfg(not(feature = "ABCI"))]
    fn test_more_than_one_vext_digest_rejected() {
        const LAST_HEIGHT: BlockHeight = BlockHeight(2);
        let (mut shell, _, _) = test_utils::setup();
        shell.storage.last_height = LAST_HEIGHT;
        let (protocol_key, _) = wallet::defaults::validator_keys();
        let vote_extension_digest = {
            let validator_addr = wallet::defaults::validator_address();
            let signed_vote_extension = {
                let ext = ethereum_events::Vext::empty(
                    LAST_HEIGHT,
                    validator_addr.clone(),
                )
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };
            // Ethereum events digest with no observed events
            ethereum_events::VextDigest {
                signatures: {
                    let mut s = HashMap::new();
                    s.insert(validator_addr, signed_vote_extension.sig);
                    s
                },
                events: vec![],
            }
        };
        let tx = ProtocolTxType::EthereumEvents(vote_extension_digest)
            .sign(&protocol_key)
            .to_bytes();
        #[allow(clippy::redundant_clone)]
        let request = ProcessProposal {
            txs: vec![tx.clone(), tx],
        };
        let results = shell.process_proposal(request);
        assert_matches!(
            results, Err(TestError::RejectProposal(s)) if s.len() == 2
        );
    }

    fn check_rejected_digest(
        shell: &mut TestShell,
        vote_extension_digest: ethereum_events::VextDigest,
        protocol_key: common::SecretKey,
    ) {
        let tx = ProtocolTxType::EthereumEvents(vote_extension_digest)
            .sign(&protocol_key)
            .to_bytes();
        let request = ProcessProposal { txs: vec![tx] };
        let response = if let Err(TestError::RejectProposal(resp)) =
            shell.process_proposal(request)
        {
            if let [resp] = resp.as_slice() {
                resp.clone()
            } else {
                panic!("Test failed")
            }
        } else {
            panic!("Test failed")
        };
        assert_eq!(
            response.result.code,
            u32::from(ErrorCodes::InvalidVoteExtension)
        );
    }

    /// Test that if a proposal contains Ethereum events with
    /// invalid validator signatures, we reject it.
    #[test]
    #[cfg(not(feature = "ABCI"))]
    fn test_drop_vext_digest_with_invalid_sigs() {
        const LAST_HEIGHT: BlockHeight = BlockHeight(2);
        let (mut shell, _, _) = test_utils::setup();
        shell.storage.last_height = LAST_HEIGHT;
        let (protocol_key, _) = wallet::defaults::validator_keys();
        let vote_extension_digest = {
            let addr = wallet::defaults::validator_address();
            let event = EthereumEvent::TransfersToNamada {
                nonce: 1u64.into(),
                transfers: vec![],
            };
            let ext = {
                // generate a valid signature
                let mut ext = ethereum_events::Vext {
                    validator_addr: addr.clone(),
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![event.clone()],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());

                // modify this signature such that it becomes invalid
                ext.sig = test_utils::invalidate_signature(ext.sig);
                ext
            };
            ethereum_events::VextDigest {
                signatures: {
                    let mut s = HashMap::new();
                    s.insert(addr.clone(), ext.sig);
                    s
                },
                events: vec![MultiSignedEthEvent {
                    event,
                    signers: {
                        let mut s = HashSet::new();
                        s.insert(addr);
                        s
                    },
                }],
            }
        };
        check_rejected_digest(&mut shell, vote_extension_digest, protocol_key);
    }

    /// Test that if a proposal contains Ethereum events with
    /// invalid block heights, we reject it.
    #[test]
    #[cfg(not(feature = "ABCI"))]
    fn test_drop_vext_digest_with_invalid_bheights() {
        const LAST_HEIGHT: BlockHeight = BlockHeight(3);
        const PRED_LAST_HEIGHT: BlockHeight = BlockHeight(LAST_HEIGHT.0 - 1);
        let (mut shell, _, _) = test_utils::setup();
        shell.storage.last_height = LAST_HEIGHT;
        let (protocol_key, _) = wallet::defaults::validator_keys();
        let vote_extension_digest = {
            let addr = wallet::defaults::validator_address();
            let event = EthereumEvent::TransfersToNamada {
                nonce: 1u64.into(),
                transfers: vec![],
            };
            let ext = {
                let ext = ethereum_events::Vext {
                    validator_addr: addr.clone(),
                    block_height: PRED_LAST_HEIGHT,
                    ethereum_events: vec![event.clone()],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };
            ethereum_events::VextDigest {
                signatures: {
                    let mut s = HashMap::new();
                    s.insert(addr.clone(), ext.sig);
                    s
                },
                events: vec![MultiSignedEthEvent {
                    event,
                    signers: {
                        let mut s = HashSet::new();
                        s.insert(addr);
                        s
                    },
                }],
            }
        };
        check_rejected_digest(&mut shell, vote_extension_digest, protocol_key);
    }

    /// Test that if a proposal contains Ethereum events with
    /// invalid validators, we reject it.
    #[test]
    #[cfg(not(feature = "ABCI"))]
    fn test_drop_vext_digest_with_invalid_validators() {
        const LAST_HEIGHT: BlockHeight = BlockHeight(2);
        let (mut shell, _, _) = test_utils::setup();
        shell.storage.last_height = LAST_HEIGHT;
        let (addr, protocol_key) = {
            let bertha_key = wallet::defaults::bertha_keypair();
            let bertha_addr = wallet::defaults::bertha_address();
            (bertha_addr, bertha_key)
        };
        let vote_extension_digest = {
            let event = EthereumEvent::TransfersToNamada {
                nonce: 1u64.into(),
                transfers: vec![],
            };
            let ext = {
                let ext = ethereum_events::Vext {
                    validator_addr: addr.clone(),
                    block_height: LAST_HEIGHT,
                    ethereum_events: vec![event.clone()],
                }
                .sign(&protocol_key);
                assert!(ext.verify(&protocol_key.ref_to()).is_ok());
                ext
            };
            ethereum_events::VextDigest {
                signatures: {
                    let mut s = HashMap::new();
                    s.insert(addr.clone(), ext.sig);
                    s
                },
                events: vec![MultiSignedEthEvent {
                    event,
                    signers: {
                        let mut s = HashSet::new();
                        s.insert(addr);
                        s
                    },
                }],
            }
        };
        check_rejected_digest(&mut shell, vote_extension_digest, protocol_key);
    }

    /// Test that if a wrapper tx is not signed, it is rejected
    /// by [`process_proposal`].
    #[test]
    fn test_unsigned_wrapper_rejected() {
        let (mut shell, _, _) = TestShell::new();
        let keypair = gen_keypair();
        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let wrapper = WrapperTx::new(
            Fee {
                amount: 0.into(),
                token: xan(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            tx,
            Default::default(),
        );
        let tx = Tx::new(
            vec![],
            Some(TxType::Wrapper(wrapper).try_to_vec().expect("Test failed")),
        )
        .to_bytes();
        #[allow(clippy::redundant_clone)]
        let request = ProcessProposal {
            txs: vec![tx.clone()],
        };

        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::InvalidSig));
        assert_eq!(
            response.result.info,
            String::from("Wrapper transactions must be signed")
        );
        #[cfg(feature = "ABCI")]
        {
            assert_eq!(response.tx, tx);
            assert!(shell.shell.storage.tx_queue.is_empty())
        }
    }

    /// Test that a wrapper tx with invalid signature is rejected
    #[test]
    fn test_wrapper_bad_signature_rejected() {
        let (mut shell, _, _) = TestShell::new();
        let keypair = gen_keypair();
        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let timestamp = tx.timestamp;
        let mut wrapper = WrapperTx::new(
            Fee {
                amount: 100.into(),
                token: xan(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            tx,
            Default::default(),
        )
        .sign(&keypair)
        .expect("Test failed");
        let new_tx = if let Some(Ok(SignedTxData {
            data: Some(data),
            sig,
        })) = wrapper
            .data
            .take()
            .map(|data| SignedTxData::try_from_slice(&data[..]))
        {
            let mut new_wrapper = if let TxType::Wrapper(wrapper) =
                <TxType as BorshDeserialize>::deserialize(&mut data.as_ref())
                    .expect("Test failed")
            {
                wrapper
            } else {
                panic!("Test failed")
            };

            // we mount a malleability attack to try and remove the fee
            new_wrapper.fee.amount = 0.into();
            let new_data = TxType::Wrapper(new_wrapper)
                .try_to_vec()
                .expect("Test failed");
            Tx {
                code: vec![],
                data: Some(
                    SignedTxData {
                        sig,
                        data: Some(new_data),
                    }
                    .try_to_vec()
                    .expect("Test failed"),
                ),
                timestamp,
            }
        } else {
            panic!("Test failed");
        };
        let request = ProcessProposal {
            txs: vec![new_tx.to_bytes()],
        };
        let response = if let [response] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            response.clone()
        } else {
            panic!("Test failed")
        };
        let expected_error = "Signature verification failed: Invalid signature";
        assert_eq!(response.result.code, u32::from(ErrorCodes::InvalidSig));
        assert!(
            response.result.info.contains(expected_error),
            "Result info {} doesn't contain the expected error {}",
            response.result.info,
            expected_error
        );
        #[cfg(feature = "ABCI")]
        {
            assert_eq!(response.tx, new_tx.to_bytes());
            assert!(shell.shell.storage.tx_queue.is_empty())
        }
    }

    /// Test that if the account submitting the tx is not known and the fee is
    /// non-zero, [`process_proposal`] rejects that tx
    #[test]
    fn test_wrapper_unknown_address() {
        let (mut shell, _, _) = TestShell::new();
        let keypair = crate::wallet::defaults::keys().remove(0).1;
        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let wrapper = WrapperTx::new(
            Fee {
                amount: 1.into(),
                token: xan(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            tx,
            Default::default(),
        )
        .sign(&keypair)
        .expect("Test failed");
        let request = ProcessProposal {
            txs: vec![wrapper.to_bytes()],
        };
        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::InvalidTx));
        assert_eq!(
            response.result.info,
            "The address given does not have sufficient balance to pay fee"
                .to_string(),
        );
        #[cfg(feature = "ABCI")]
        {
            assert_eq!(response.tx, wrapper.to_bytes());
            assert!(shell.shell.storage.tx_queue.is_empty())
        }
    }

    /// Test that if the account submitting the tx does
    /// not have sufficient balance to pay the fee,
    /// [`process_proposal`] rejects that tx
    #[test]
    fn test_wrapper_insufficient_balance_address() {
        let (mut shell, _, _) = TestShell::new();
        shell.init_chain(RequestInitChain {
            time: Some(Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            chain_id: ChainId::default().to_string(),
            ..Default::default()
        });
        let keypair = crate::wallet::defaults::daewon_keypair();

        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let wrapper = WrapperTx::new(
            Fee {
                amount: Amount::whole(1_000_100),
                token: xan(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            tx,
            Default::default(),
        )
        .sign(&keypair)
        .expect("Test failed");

        let request = ProcessProposal {
            txs: vec![wrapper.to_bytes()],
        };

        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::InvalidTx));
        assert_eq!(
            response.result.info,
            String::from(
                "The address given does not have sufficient balance to pay fee"
            )
        );
        #[cfg(feature = "ABCI")]
        {
            assert_eq!(response.tx, wrapper.to_bytes());
            assert!(shell.shell.storage.tx_queue.is_empty())
        }
    }

    #[cfg(not(feature = "ABCI"))]
    /// Test that if the expected order of decrypted txs is
    /// validated, [`process_proposal`] rejects it
    #[test]
    fn test_decrypted_txs_out_of_order() {
        let (mut shell, _, _) = TestShell::new();
        let keypair = gen_keypair();
        let mut txs = vec![];
        for i in 0..3 {
            let tx = Tx::new(
                "wasm_code".as_bytes().to_owned(),
                Some(format!("transaction data: {}", i).as_bytes().to_owned()),
            );
            let wrapper = WrapperTx::new(
                Fee {
                    amount: i.into(),
                    token: xan(),
                },
                &keypair,
                Epoch(0),
                0.into(),
                tx.clone(),
                Default::default(),
            );
            shell.enqueue_tx(wrapper);
            txs.push(Tx::from(TxType::Decrypted(DecryptedTx::Decrypted(tx))));
        }
        let req_1 = ProcessProposal {
            txs: vec![txs[0].to_bytes()],
        };
        let response_1 = if let [resp] = shell
            .process_proposal(req_1)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response_1.result.code, u32::from(ErrorCodes::Ok));

        let req_2 = ProcessProposal {
            txs: vec![txs[2].to_bytes()],
        };

        let response_2 = if let Err(TestError::RejectProposal(resp)) =
            shell.process_proposal(req_2)
        {
            if let [resp] = resp.as_slice() {
                resp.clone()
            } else {
                panic!("Test failed")
            }
        } else {
            panic!("Test failed")
        };
        assert_eq!(response_2.result.code, u32::from(ErrorCodes::InvalidOrder));
        assert_eq!(
            response_2.result.info,
            String::from(
                "Process proposal rejected a decrypted transaction that \
                 violated the tx order determined in the previous block"
            ),
        );
    }

    #[cfg(not(feature = "ABCI"))]
    /// Test that a tx incorrectly labelled as undecryptable
    /// is rejected by [`process_proposal`]
    #[test]
    fn test_incorrectly_labelled_as_undecryptable() {
        let (mut shell, _, _) = TestShell::new();
        let keypair = gen_keypair();

        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let wrapper = WrapperTx::new(
            Fee {
                amount: 0.into(),
                token: xan(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            tx,
            Default::default(),
        );
        shell.enqueue_tx(wrapper.clone());

        let tx =
            Tx::from(TxType::Decrypted(DecryptedTx::Undecryptable(wrapper)));

        let request = ProcessProposal {
            txs: vec![tx.to_bytes()],
        };

        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::InvalidTx));
        assert_eq!(
            response.result.info,
            String::from(
                "The encrypted payload of tx was incorrectly marked as \
                 un-decryptable"
            ),
        )
    }

    /// Test that a wrapper tx whose inner_tx does not have
    /// the same hash as the wrappers tx_hash field is marked
    /// undecryptable but still accepted
    #[test]
    fn test_invalid_hash_commitment() {
        let (mut shell, _, _) = TestShell::new();
        shell.init_chain(RequestInitChain {
            time: Some(Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            chain_id: ChainId::default().to_string(),
            ..Default::default()
        });
        let keypair = crate::wallet::defaults::daewon_keypair();

        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let mut wrapper = WrapperTx::new(
            Fee {
                amount: 0.into(),
                token: xan(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            tx,
            Default::default(),
        );
        wrapper.tx_hash = Hash([0; 32]);

        let tx = if !cfg!(feature = "ABCI") {
            shell.enqueue_tx(wrapper.clone());
            Tx::from(TxType::Decrypted(DecryptedTx::Undecryptable(
                #[allow(clippy::redundant_clone)]
                wrapper.clone(),
            )))
        } else {
            wrapper.sign(&keypair).expect("Test failed")
        };

        let request = ProcessProposal {
            txs: vec![tx.to_bytes()],
        };
        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::Ok));
        #[cfg(feature = "ABCI")]
        {
            match process_tx(
                Tx::try_from(response.tx.as_ref()).expect("Test failed"),
            )
            .expect("Test failed")
            {
                TxType::Decrypted(DecryptedTx::Undecryptable(inner)) => {
                    assert_eq!(
                        hash_tx(inner.try_to_vec().unwrap().as_ref()),
                        hash_tx(wrapper.try_to_vec().unwrap().as_ref())
                    );
                    assert!(shell.shell.storage.tx_queue.is_empty())
                }
                _ => panic!("Test failed"),
            }
        }
    }

    /// Test that if a wrapper tx contains garbage bytes
    /// as its encrypted inner tx, it is correctly
    /// marked undecryptable and the errors handled correctly
    #[test]
    fn test_undecryptable() {
        let (mut shell, _, _) = TestShell::new();
        shell.init_chain(RequestInitChain {
            time: Some(Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            chain_id: ChainId::default().to_string(),
            ..Default::default()
        });
        let keypair = crate::wallet::defaults::daewon_keypair();
        let pubkey = EncryptionKey::default();
        // not valid tx bytes
        let tx = "garbage data".as_bytes().to_owned();
        let inner_tx = EncryptedTx::encrypt(&tx, pubkey);
        let wrapper = WrapperTx {
            fee: Fee {
                amount: 0.into(),
                token: xan(),
            },
            pk: keypair.ref_to(),
            epoch: Epoch(0),
            gas_limit: 0.into(),
            inner_tx,
            tx_hash: hash_tx(&tx),
        };

        let signed = if !cfg!(feature = "ABCI") {
            shell.enqueue_tx(wrapper.clone());
            Tx::from(TxType::Decrypted(DecryptedTx::Undecryptable(
                #[allow(clippy::redundant_clone)]
                wrapper.clone(),
            )))
        } else {
            wrapper.sign(&keypair).expect("Test failed")
        };
        let request = ProcessProposal {
            txs: vec![signed.to_bytes()],
        };
        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::Ok));
        #[cfg(feature = "ABCI")]
        {
            match process_tx(
                Tx::try_from(response.tx.as_ref()).expect("Test failed"),
            )
            .expect("Test failed")
            {
                TxType::Decrypted(DecryptedTx::Undecryptable(inner)) => {
                    assert_eq!(
                        hash_tx(inner.try_to_vec().unwrap().as_ref()),
                        hash_tx(wrapper.try_to_vec().unwrap().as_ref())
                    );
                    assert!(shell.shell.storage.tx_queue.is_empty());
                }
                _ => panic!("Test failed"),
            }
        }
    }

    #[cfg(not(feature = "ABCI"))]
    /// Test that if more decrypted txs are submitted to
    /// [`process_proposal`] than expected, they are rejected
    #[test]
    fn test_too_many_decrypted_txs() {
        let (mut shell, _, _) = TestShell::new();

        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );

        let tx = Tx::from(TxType::Decrypted(DecryptedTx::Decrypted(tx)));

        let request = ProcessProposal {
            txs: vec![tx.to_bytes()],
        };
        let response = if let Err(TestError::RejectProposal(resp)) =
            shell.process_proposal(request)
        {
            if let [resp] = resp.as_slice() {
                resp.clone()
            } else {
                panic!("Test failed")
            }
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::ExtraTxs));
        assert_eq!(
            response.result.info,
            String::from("Received more decrypted txs than expected"),
        );
    }

    /// Process Proposal should reject a RawTx, but not panic
    #[test]
    fn test_raw_tx_rejected() {
        let (mut shell, _, _) = TestShell::new();

        let tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some("transaction data".as_bytes().to_owned()),
        );
        let tx = Tx::from(TxType::Raw(tx));
        let request = ProcessProposal {
            txs: vec![tx.to_bytes()],
        };
        let response = if let [resp] = shell
            .process_proposal(request)
            .expect("Test failed")
            .as_slice()
        {
            resp.clone()
        } else {
            panic!("Test failed")
        };
        assert_eq!(response.result.code, u32::from(ErrorCodes::InvalidTx));
        assert_eq!(
            response.result.info,
            String::from(
                "Transaction rejected: Non-encrypted transactions are not \
                 supported"
            ),
        );
        #[cfg(feature = "ABCI")]
        {
            assert_eq!(response.tx, tx.to_bytes());
        }
    }
}
