//! Implementation of the `FinalizeBlock` ABCI++ method for the Shell

use namada::ledger::protocol;
use namada::types::storage::{BlockHash, Header};
use namada::types::transaction::protocol::ProtocolTxType;

use super::governance::execute_governance_proposals;
use super::*;
use crate::facade::tendermint_proto::abci::Misbehavior as Evidence;
use crate::facade::tendermint_proto::crypto::PublicKey as TendermintPublicKey;

impl<D, H> Shell<D, H>
where
    D: DB + for<'iter> DBIter<'iter> + Sync + 'static,
    H: StorageHasher + Sync + 'static,
{
    /// Updates the chain with new header, height, etc. Also keeps track
    /// of epoch changes and applies associated updates to validator sets,
    /// etc. as necessary.
    ///
    /// Validate and apply decrypted transactions unless
    /// [`Shell::process_proposal`] detected that they were not submitted in
    /// correct order or more decrypted txs arrived than expected. In that
    /// case, all decrypted transactions are not applied and must be
    /// included in the next `Shell::prepare_proposal` call.
    ///
    /// Incoming wrapper txs need no further validation. They
    /// are added to the block.
    ///
    /// Error codes:
    ///   0: Ok
    ///   1: Invalid tx
    ///   2: Tx is invalidly signed
    ///   3: Wasm runtime error
    ///   4: Invalid order of decrypted txs
    ///   5. More decrypted txs than expected
    pub fn finalize_block(
        &mut self,
        req: shim::request::FinalizeBlock,
    ) -> Result<shim::response::FinalizeBlock> {
        // reset gas meter before we start
        self.gas_meter.reset();

        let mut response = shim::response::FinalizeBlock::default();
        // begin the next block and check if a new epoch began
        let (height, new_epoch) =
            self.update_state(req.header, req.hash, req.byzantine_validators);

        if new_epoch {
            let _proposals_result =
                execute_governance_proposals(self, &mut response)?;
        }

        for processed_tx in &req.txs {
            let tx = if let Ok(tx) = Tx::try_from(processed_tx.tx.as_ref()) {
                tx
            } else {
                tracing::error!(
                    "FinalizeBlock received a tx that could not be \
                     deserialized to a Tx type. This is likely a protocol \
                     transaction."
                );
                continue;
            };
            let tx_length = processed_tx.tx.len();
            // If [`process_proposal`] rejected a Tx due to invalid signature,
            // emit an event here and move on to next tx.
            if ErrorCodes::from_u32(processed_tx.result.code).unwrap()
                == ErrorCodes::InvalidSig
            {
                let mut tx_event = match process_tx(tx.clone()) {
                    Ok(tx @ TxType::Wrapper(_))
                    | Ok(tx @ TxType::Protocol(_)) => {
                        Event::new_tx_event(&tx, height.0)
                    }
                    _ => match TxType::try_from(tx) {
                        Ok(tx @ TxType::Wrapper(_))
                        | Ok(tx @ TxType::Protocol(_)) => {
                            Event::new_tx_event(&tx, height.0)
                        }
                        _ => {
                            tracing::error!(
                                "Internal logic error: FinalizeBlock received \
                                 a tx with an invalid signature error code \
                                 that could not be deserialized to a \
                                 WrapperTx / ProtocolTx type"
                            );
                            continue;
                        }
                    },
                };
                tx_event["code"] = processed_tx.result.code.to_string();
                tx_event["info"] =
                    format!("Tx rejected: {}", &processed_tx.result.info);
                tx_event["gas_used"] = "0".into();
                response.events.push(tx_event);
                continue;
            }

            let tx_type = if let Ok(tx_type) = process_tx(tx) {
                tx_type
            } else {
                tracing::error!(
                    "Internal logic error: FinalizeBlock received tx that \
                     could not be deserialized to a valid TxType"
                );
                continue;
            };
            // If [`process_proposal`] rejected a Tx, emit an event here and
            // move on to next tx
            if ErrorCodes::from_u32(processed_tx.result.code).unwrap()
                != ErrorCodes::Ok
            {
                let mut tx_event = Event::new_tx_event(&tx_type, height.0);
                tx_event["code"] = processed_tx.result.code.to_string();
                tx_event["info"] =
                    format!("Tx rejected: {}", &processed_tx.result.info);
                tx_event["gas_used"] = "0".into();
                response.events.push(tx_event);
                // if the rejected tx was decrypted, remove it
                // from the queue of txs to be processed
                if let TxType::Decrypted(_) = &tx_type {
                    self.storage.tx_queue.pop();
                }
                continue;
            }

            let mut tx_event = match &tx_type {
                TxType::Wrapper(_wrapper) => {
                    self.storage.tx_queue.push(_wrapper.clone());
                    Event::new_tx_event(&tx_type, height.0)
                }
                TxType::Decrypted(inner) => {
                    // We remove the corresponding wrapper tx from the queue
                    self.storage.tx_queue.pop();
                    let mut event = Event::new_tx_event(&tx_type, height.0);
                    if let DecryptedTx::Undecryptable(_) = inner {
                        event["log"] =
                            "Transaction could not be decrypted.".into();
                        event["code"] = ErrorCodes::Undecryptable.into();
                    }
                    event
                }
                TxType::Raw(_) => {
                    tracing::error!(
                        "Internal logic error: FinalizeBlock received a \
                         TxType::Raw transaction"
                    );
                    continue;
                }
                TxType::Protocol(protocol_tx) => match protocol_tx.tx {
                    #[cfg(not(feature = "abcipp"))]
                    ProtocolTxType::EthEventsVext(ref ext) => {
                        for event in ext.data.ethereum_events.iter() {
                            self.mode.deque_eth_event(event);
                        }
                        Event::new_tx_event(&tx_type, height.0)
                    }
                    #[cfg(not(feature = "abcipp"))]
                    ProtocolTxType::ValSetUpdateVext(_) => {
                        Event::new_tx_event(&tx_type, height.0)
                    }
                    #[cfg(feature = "abcipp")]
                    ProtocolTxType::EthereumEvents(ref digest) => {
                        for event in
                            digest.events.iter().map(|signed| &signed.event)
                        {
                            self.mode.deque_eth_event(event);
                        }
                        Event::new_tx_event(&tx_type, height.0)
                    }
                    #[cfg(feature = "abcipp")]
                    ProtocolTxType::ValidatorSetUpdate(_) => {
                        Event::new_tx_event(&tx_type, height.0)
                    }
                    ref protocol_tx_type => {
                        tracing::error!(
                            ?protocol_tx_type,
                            "Internal logic error: FinalizeBlock received an \
                             unsupported TxType::Protocol transaction: {:?}",
                            protocol_tx
                        );
                        continue;
                    }
                },
            };

            match protocol::dispatch_tx(
                tx_type,
                tx_length,
                &mut self.gas_meter,
                &mut self.write_log,
                &mut self.storage,
                &mut self.vp_wasm_cache,
                &mut self.tx_wasm_cache,
            )
            .map_err(Error::TxApply)
            {
                Ok(result) => {
                    if result.is_accepted() {
                        tracing::info!(
                            "all VPs accepted transaction {} storage \
                             modification {:#?}",
                            tx_event["hash"],
                            result
                        );
                        self.write_log.commit_tx();
                        if !tx_event.contains_key("code") {
                            tx_event["code"] = ErrorCodes::Ok.into();
                        }
                        if let Some(ibc_event) = &result.ibc_event {
                            // Add the IBC event besides the tx_event
                            let event = Event::from(ibc_event.clone());
                            response.events.push(event);
                        }
                        match serde_json::to_string(
                            &result.initialized_accounts,
                        ) {
                            Ok(initialized_accounts) => {
                                tx_event["initialized_accounts"] =
                                    initialized_accounts;
                            }
                            Err(err) => {
                                tracing::error!(
                                    "Failed to serialize the initialized \
                                     accounts: {}",
                                    err
                                );
                            }
                        }
                    } else {
                        tracing::info!(
                            "some VPs rejected transaction {} storage \
                             modification {:#?}",
                            tx_event["hash"],
                            result.vps_result.rejected_vps
                        );
                        self.write_log.drop_tx();
                        tx_event["code"] = ErrorCodes::InvalidTx.into();
                    }
                    tx_event["gas_used"] = result.gas_used.to_string();
                    tx_event["info"] = result.to_string();
                }
                Err(msg) => {
                    tracing::info!(
                        "Transaction {} failed with: {}",
                        tx_event["hash"],
                        msg
                    );
                    self.write_log.drop_tx();
                    tx_event["gas_used"] = self
                        .gas_meter
                        .get_current_transaction_gas()
                        .to_string();
                    tx_event["info"] = msg.to_string();
                    tx_event["code"] = ErrorCodes::WasmRuntimeError.into();
                }
            }
            response.events.push(tx_event);
        }

        if new_epoch {
            self.update_epoch(&mut response);
        }

        let _ = self
            .gas_meter
            .finalize_transaction()
            .map_err(|_| Error::GasOverflow)?;

        self.event_log_mut().log_events(response.events.clone());

        Ok(response)
    }

    /// Sets the metadata necessary for a new block, including
    /// the hash, height, validator changes, and evidence of
    /// byzantine behavior. Applies slashes if necessary.
    /// Returns a bool indicating if a new epoch began and
    /// the height of the new block.
    fn update_state(
        &mut self,
        header: Header,
        hash: BlockHash,
        byzantine_validators: Vec<Evidence>,
    ) -> (BlockHeight, bool) {
        let height = self.storage.last_height + 1;

        self.gas_meter.reset();

        self.storage
            .begin_block(hash, height)
            .expect("Beginning a block shouldn't fail");

        self.storage
            .set_header(header)
            .expect("Setting a header shouldn't fail");

        self.byzantine_validators = byzantine_validators;

        let header = self
            .storage
            .header
            .as_ref()
            .expect("Header must have been set in prepare_proposal.");
        let time = header.time;
        let new_epoch = self
            .storage
            .update_epoch(height, time)
            .expect("Must be able to update epoch");

        self.slash();
        (height, new_epoch)
    }

    /// If a new epoch begins, we update the response to include
    /// changes to the validator sets and consensus parameters
    fn update_epoch(&self, response: &mut shim::response::FinalizeBlock) {
        // Apply validator set update
        let (current_epoch, _gas) = self.storage.get_current_epoch();
        // TODO ABCI validator updates on block H affects the validator set
        // on block H+2, do we need to update a block earlier?
        self.storage.validator_set_update(current_epoch, |update| {
            let (consensus_key, power) = match update {
                ValidatorSetUpdate::Active(ActiveValidator {
                    consensus_key,
                    voting_power,
                }) => {
                    let power: u64 = voting_power.into();
                    let power: i64 = power
                        .try_into()
                        .expect("unexpected validator's voting power");
                    (consensus_key, power)
                }
                ValidatorSetUpdate::Deactivated(consensus_key) => {
                    // Any validators that have become inactive must
                    // have voting power set to 0 to remove them from
                    // the active set
                    let power = 0_i64;
                    (consensus_key, power)
                }
            };
            let pub_key = TendermintPublicKey {
                sum: Some(key_to_tendermint(&consensus_key).unwrap()),
            };
            let pub_key = Some(pub_key);
            let update = ValidatorUpdate { pub_key, power };
            response.validator_updates.push(update);
        });
    }
}

/// We test the failure cases of [`finalize_block`]. The happy flows
/// are covered by the e2e tests.
#[cfg(test)]
mod test_finalize_block {
    use namada::types::address::nam;
    use namada::types::ethereum_events::EthAddress;
    use namada::types::storage::Epoch;
    use namada::types::transaction::{EncryptionKey, Fee};
    use namada::types::vote_extensions::ethereum_events;
    #[cfg(feature = "abcipp")]
    use namada::types::vote_extensions::ethereum_events::MultiSignedEthEvent;

    use super::*;
    use crate::node::ledger::shell::test_utils::*;
    use crate::node::ledger::shims::abcipp_shim_types::shim::request::{
        FinalizeBlock, ProcessedTx,
    };

    /// Check that if a wrapper tx was rejected by [`process_proposal`],
    /// check that the correct event is returned. Check that it does
    /// not appear in the queue of txs to be decrypted
    #[test]
    fn test_process_proposal_rejected_wrapper_tx() {
        let (mut shell, _, _) = setup();
        let keypair = gen_keypair();
        let mut processed_txs = vec![];
        let mut valid_wrappers = vec![];
        // create some wrapper txs
        for i in 1..5 {
            let raw_tx = Tx::new(
                "wasm_code".as_bytes().to_owned(),
                Some(format!("transaction data: {}", i).as_bytes().to_owned()),
            );
            let wrapper = WrapperTx::new(
                Fee {
                    amount: i.into(),
                    token: nam(),
                },
                &keypair,
                Epoch(0),
                0.into(),
                raw_tx.clone(),
                Default::default(),
            );
            let tx = wrapper.sign(&keypair).expect("Test failed");
            if i > 1 {
                processed_txs.push(ProcessedTx {
                    tx: tx.to_bytes(),
                    result: TxResult {
                        code: u32::try_from(i.rem_euclid(2))
                            .expect("Test failed"),
                        info: "".into(),
                    },
                });
            } else {
                shell.enqueue_tx(wrapper.clone());
            }

            if i != 3 {
                valid_wrappers.push(wrapper)
            }
        }

        // check that the correct events were created
        for (index, event) in shell
            .finalize_block(FinalizeBlock {
                txs: processed_txs.clone(),
                ..Default::default()
            })
            .expect("Test failed")
            .iter()
            .enumerate()
        {
            assert_eq!(event.event_type.to_string(), String::from("accepted"));
            let code = event.attributes.get("code").expect("Test failed");
            assert_eq!(code, &index.rem_euclid(2).to_string());
        }
        // verify that the queue of wrapper txs to be processed is correct
        let mut valid_tx = valid_wrappers.iter();
        let mut counter = 0;
        for wrapper in shell.iter_tx_queue() {
            // we cannot easily implement the PartialEq trait for WrapperTx
            // so we check the hashes of the inner txs for equality
            assert_eq!(
                wrapper.tx_hash,
                valid_tx.next().expect("Test failed").tx_hash
            );
            counter += 1;
        }
        assert_eq!(counter, 3);
    }

    /// Check that if a decrypted tx was rejected by [`process_proposal`],
    /// check that the correct event is returned. Check that it is still
    /// removed from the queue of txs to be included in the next block
    /// proposal
    #[test]
    fn test_process_proposal_rejected_decrypted_tx() {
        let (mut shell, _, _) = setup();
        let keypair = gen_keypair();
        let raw_tx = Tx::new(
            "wasm_code".as_bytes().to_owned(),
            Some(String::from("transaction data").as_bytes().to_owned()),
        );
        let wrapper = WrapperTx::new(
            Fee {
                amount: 0.into(),
                token: nam(),
            },
            &keypair,
            Epoch(0),
            0.into(),
            raw_tx.clone(),
            Default::default(),
        );

        let processed_tx = ProcessedTx {
            tx: Tx::from(TxType::Decrypted(DecryptedTx::Decrypted(raw_tx)))
                .to_bytes(),
            result: TxResult {
                code: ErrorCodes::InvalidTx.into(),
                info: "".into(),
            },
        };
        shell.enqueue_tx(wrapper);

        // check that the decrypted tx was not applied
        for event in shell
            .finalize_block(FinalizeBlock {
                txs: vec![processed_tx],
                ..Default::default()
            })
            .expect("Test failed")
        {
            assert_eq!(event.event_type.to_string(), String::from("applied"));
            let code = event.attributes.get("code").expect("Test failed");
            assert_eq!(code, &String::from(ErrorCodes::InvalidTx));
        }
        // check that the corresponding wrapper tx was removed from the queue
        assert!(shell.storage.tx_queue.is_empty());
    }

    /// Test that if a tx is undecryptable, it is applied
    /// but the tx result contains the appropriate error code.
    #[test]
    fn test_undecryptable_returns_error_code() {
        let (mut shell, _, _) = setup();

        let keypair = crate::wallet::defaults::daewon_keypair();
        let pubkey = EncryptionKey::default();
        // not valid tx bytes
        let tx = "garbage data".as_bytes().to_owned();
        let inner_tx =
            namada::types::transaction::encrypted::EncryptedTx::encrypt(
                &tx, pubkey,
            );
        let wrapper = WrapperTx {
            fee: Fee {
                amount: 0.into(),
                token: nam(),
            },
            pk: keypair.ref_to(),
            epoch: Epoch(0),
            gas_limit: 0.into(),
            inner_tx,
            tx_hash: hash_tx(&tx),
        };
        let processed_tx = ProcessedTx {
            tx: Tx::from(TxType::Decrypted(DecryptedTx::Undecryptable(
                wrapper.clone(),
            )))
            .to_bytes(),
            result: TxResult {
                code: ErrorCodes::Ok.into(),
                info: "".into(),
            },
        };

        shell.enqueue_tx(wrapper);

        // check that correct error message is returned
        for event in shell
            .finalize_block(FinalizeBlock {
                txs: vec![processed_tx],
                ..Default::default()
            })
            .expect("Test failed")
        {
            assert_eq!(event.event_type.to_string(), String::from("applied"));
            let code = event.attributes.get("code").expect("Test failed");
            assert_eq!(code, &String::from(ErrorCodes::Undecryptable));
            let log = event.attributes.get("log").expect("Test failed");
            assert!(log.contains("Transaction could not be decrypted."))
        }
        // check that the corresponding wrapper tx was removed from the queue
        assert!(shell.storage.tx_queue.is_empty());
    }

    /// Test that the wrapper txs are queued in the order they
    /// are received from the block. Tests that the previously
    /// decrypted txs are de-queued.
    #[test]
    fn test_mixed_txs_queued_in_correct_order() {
        let (mut shell, _, _) = setup();
        let keypair = gen_keypair();
        let mut processed_txs = vec![];
        let mut valid_txs = vec![];

        // create two decrypted txs
        let mut wasm_path = top_level_directory();
        wasm_path.push("wasm_for_tests/tx_no_op.wasm");
        let tx_code = std::fs::read(wasm_path)
            .expect("Expected a file at given code path");
        for i in 0..2 {
            let raw_tx = Tx::new(
                tx_code.clone(),
                Some(
                    format!("Decrypted transaction data: {}", i)
                        .as_bytes()
                        .to_owned(),
                ),
            );
            let wrapper_tx = WrapperTx::new(
                Fee {
                    amount: 0.into(),
                    token: nam(),
                },
                &keypair,
                Epoch(0),
                0.into(),
                raw_tx.clone(),
                Default::default(),
            );
            shell.enqueue_tx(wrapper_tx);
            processed_txs.push(ProcessedTx {
                tx: Tx::from(TxType::Decrypted(DecryptedTx::Decrypted(raw_tx)))
                    .to_bytes(),
                result: TxResult {
                    code: ErrorCodes::Ok.into(),
                    info: "".into(),
                },
            });
        }
        // create two wrapper txs
        for i in 0..2 {
            let raw_tx = Tx::new(
                "wasm_code".as_bytes().to_owned(),
                Some(
                    format!("Encrypted transaction data: {}", i)
                        .as_bytes()
                        .to_owned(),
                ),
            );
            let wrapper_tx = WrapperTx::new(
                Fee {
                    amount: 0.into(),
                    token: nam(),
                },
                &keypair,
                Epoch(0),
                0.into(),
                raw_tx.clone(),
                Default::default(),
            );
            let wrapper = wrapper_tx.sign(&keypair).expect("Test failed");
            valid_txs.push(wrapper_tx);
            processed_txs.push(ProcessedTx {
                tx: wrapper.to_bytes(),
                result: TxResult {
                    code: ErrorCodes::Ok.into(),
                    info: "".into(),
                },
            });
        }
        // Put the wrapper txs in front of the decrypted txs
        processed_txs.rotate_left(2);
        // check that the correct events were created
        for (index, event) in shell
            .finalize_block(FinalizeBlock {
                txs: processed_txs,
                ..Default::default()
            })
            .expect("Test failed")
            .iter()
            .enumerate()
        {
            if index < 2 {
                // these should be accepted wrapper txs
                assert_eq!(
                    event.event_type.to_string(),
                    String::from("accepted")
                );
                let code =
                    event.attributes.get("code").expect("Test failed").as_str();
                assert_eq!(code, String::from(ErrorCodes::Ok).as_str());
            } else {
                // these should be accepted decrypted txs
                assert_eq!(
                    event.event_type.to_string(),
                    String::from("applied")
                );
                let code =
                    event.attributes.get("code").expect("Test failed").as_str();
                assert_eq!(code, String::from(ErrorCodes::Ok).as_str());
            }
        }

        // check that the applied decrypted txs were dequeued and the
        // accepted wrappers were enqueued in correct order
        let mut txs = valid_txs.iter();

        let mut counter = 0;
        for wrapper in shell.iter_tx_queue() {
            assert_eq!(
                wrapper.tx_hash,
                txs.next().expect("Test failed").tx_hash
            );
            counter += 1;
        }
        assert_eq!(counter, 2);
    }

    /// Test if a rejected protocol tx is applied and emits
    /// the correct event
    #[test]
    fn test_rejected_protocol_tx() {
        const LAST_HEIGHT: BlockHeight = BlockHeight(3);
        let (mut shell, _, _) = setup_at_height(LAST_HEIGHT);
        let protocol_key =
            shell.mode.get_protocol_key().expect("Test failed").clone();

        #[cfg(feature = "abcipp")]
        let tx = ProtocolTxType::EthereumEvents(ethereum_events::VextDigest {
            signatures: Default::default(),
            events: vec![],
        })
        .sign(&protocol_key)
        .to_bytes();

        #[cfg(not(feature = "abcipp"))]
        let tx = ProtocolTxType::EthEventsVext(
            ethereum_events::Vext::empty(
                LAST_HEIGHT,
                shell
                    .mode
                    .get_validator_address()
                    .expect("Test failed")
                    .clone(),
            )
            .sign(&protocol_key),
        )
        .sign(&protocol_key)
        .to_bytes();

        let req = FinalizeBlock {
            txs: vec![ProcessedTx {
                tx,
                result: TxResult {
                    code: ErrorCodes::InvalidTx.into(),
                    info: Default::default(),
                },
            }],
            ..Default::default()
        };
        let mut resp = shell.finalize_block(req).expect("Test failed");
        assert_eq!(resp.len(), 1);
        let event = resp.remove(0);
        assert_eq!(event.event_type.to_string(), String::from("applied"));
        let code = event.attributes.get("code").expect("Test failed");
        assert_eq!(code, &String::from(ErrorCodes::InvalidTx));
    }

    /// Test that once a validator's vote for an Ethereum event lands
    /// on-chain, it dequeues from the list of events to vote on.
    #[test]
    fn test_eth_events_dequeued() {
        let (mut shell, _, oracle) = setup();
        let protocol_key =
            shell.mode.get_protocol_key().expect("Test failed").clone();
        let address = shell
            .mode
            .get_validator_address()
            .expect("Test failed")
            .clone();

        // ---- the ledger receives a new Ethereum event
        let event = EthereumEvent::NewContract {
            name: "Test".to_string(),
            address: EthAddress([0; 20]),
        };
        tokio_test::block_on(oracle.send(event.clone())).expect("Test failed");
        let [queued_event]: [EthereumEvent; 1] =
            shell.new_ethereum_events().try_into().expect("Test failed");
        assert_eq!(queued_event, event);

        // ---- The protocol tx that includes this event on-chain
        #[allow(clippy::redundant_clone)]
        let ext = ethereum_events::Vext {
            block_height: shell.storage.last_height,
            ethereum_events: vec![event.clone()],
            validator_addr: address.clone(),
        }
        .sign(&protocol_key);

        #[cfg(feature = "abcipp")]
        let processed_tx = {
            let signed = MultiSignedEthEvent {
                event,
                #[cfg(feature = "abcipp")]
                signers: BTreeSet::from([address.clone()]),
                #[cfg(not(feature = "abcipp"))]
                signers: BTreeSet::from([(
                    address.clone(),
                    shell.storage.last_height,
                )]),
            };

            let digest = ethereum_events::VextDigest {
                #[cfg(feature = "abcipp")]
                signatures: vec![(address, ext.sig)].into_iter().collect(),
                #[cfg(not(feature = "abcipp"))]
                signatures: vec![(
                    (address, shell.storage.last_height),
                    ext.sig,
                )]
                .into_iter()
                .collect(),
                events: vec![signed],
            };
            ProcessedTx {
                tx: ProtocolTxType::EthereumEvents(digest)
                    .sign(&protocol_key)
                    .to_bytes(),
                result: TxResult {
                    code: ErrorCodes::Ok.into(),
                    info: "".into(),
                },
            }
        };

        #[cfg(not(feature = "abcipp"))]
        let processed_tx = ProcessedTx {
            tx: ProtocolTxType::EthEventsVext(ext)
                .sign(&protocol_key)
                .to_bytes(),
            result: TxResult {
                code: ErrorCodes::Ok.into(),
                info: "".into(),
            },
        };

        // ---- This protocol tx is accepted
        let [result]: [Event; 1] = shell
            .finalize_block(FinalizeBlock {
                txs: vec![processed_tx],
                ..Default::default()
            })
            .expect("Test failed")
            .try_into()
            .expect("Test failed");
        assert_eq!(result.event_type.to_string(), String::from("applied"));
        let code = result.attributes.get("code").expect("Test failed").as_str();
        assert_eq!(code, String::from(ErrorCodes::Ok).as_str());

        // --- The event is removed from the queue
        assert!(shell.new_ethereum_events().is_empty());
    }
}
