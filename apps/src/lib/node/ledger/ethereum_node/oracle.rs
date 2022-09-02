#[cfg(feature = "eth-fullnode")]
pub mod oracle_process {
    use std::ops::Deref;

    use clarity::Address;
    use namada::types::ethereum_events::{EthAddress, EthereumEvent};
    use num256::Uint256;
    use tokio::sync::mpsc::UnboundedSender;
    use tokio::sync::oneshot::Sender;
    use tokio::task::LocalSet;
    #[cfg(not(test))]
    use web30::client::Web3;

    use super::super::events::{signatures, PendingEvent};
    #[cfg(test)]
    use super::super::test_tools::mock_web3_client::Web3;

    /// Minimum number of confirmations needed to trust an Ethereum branch
    pub(crate) const MIN_CONFIRMATIONS: u64 = 50;

    /// Dummy addresses for smart contracts
    const MINT_CONTRACT: EthAddress = EthAddress([0; 20]);
    const GOVERNANCE_CONTRACT: EthAddress = EthAddress([1; 20]);

    /// A client that can talk to geth and parse
    /// and relay events relevant to Anoma to the
    /// ledger process
    pub struct Oracle {
        /// The client that talks to the Ethereum fullnode
        client: Web3,
        /// A channel for sending processed and confirmed
        /// events to the ledger process
        sender: UnboundedSender<EthereumEvent>,
        /// A channel to signal that the ledger should shut down
        /// because the Oracle has stopped
        abort: Option<Sender<()>>,
    }

    impl Deref for Oracle {
        type Target = Web3;

        fn deref(&self) -> &Self::Target {
            &self.client
        }
    }

    impl Drop for Oracle {
        fn drop(&mut self) {
            // send an abort signal to shut down the
            // rest of the ledger gracefully
            let abort = self.abort.take().unwrap();
            let _ = abort.send(());
        }
    }

    impl Oracle {
        /// Initialize a new [`Oracle`]
        pub fn new(
            url: &str,
            sender: UnboundedSender<EthereumEvent>,
            abort: Sender<()>,
        ) -> Self {
            Self {
                client: Web3::new(url, std::time::Duration::from_secs(30)),
                sender,
                abort: Some(abort),
            }
        }

        /// Send a series of [`EthereumEvent`]s to the Anoma
        /// ledger. Returns a boolean indicating that all sent
        /// successfully. If false is returned, the receiver
        /// has hung up.
        fn send(&self, events: Vec<EthereumEvent>) -> bool {
            events
                .into_iter()
                .map(|event| self.sender.send(event))
                .all(|res| res.is_ok())
                && !self.sender.is_closed()
        }

        /// Check if the receiver in the ledger has hung up.
        /// Used to help determine when to stop the oracle
        fn connected(&self) -> bool {
            !self.sender.is_closed()
        }
    }

    /// Set up an Oracle and run the process where the Oracle
    /// processes and forwards Ethereum events to the ledger
    pub fn run_oracle(
        url: impl AsRef<str>,
        sender: UnboundedSender<EthereumEvent>,
        abort_sender: Sender<()>,
    ) -> tokio::task::JoinHandle<()> {
        let url = url.as_ref().to_owned();
        // we have to run the oracle in a [`LocalSet`] due to the web30
        // crate
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                LocalSet::new()
                    .run_until(async move {
                        tracing::info!(
                            ?url,
                            "Ethereum event oracle is starting"
                        );

                        let oracle = Oracle::new(&url, sender, abort_sender);
                        run_oracle_aux(oracle).await;

                        tracing::info!(
                            ?url,
                            "Ethereum event oracle is no longer running"
                        );
                    })
                    .await
            });
        })
    }

    /// Given an oracle, watch for new Ethereum events, processing
    /// them into Anoma native types.
    ///
    /// It also checks that once the specified number of confirmations
    /// is reached, an event is forwarded to the ledger process
    async fn run_oracle_aux(oracle: Oracle) {
        // Initialize our local state. This includes
        // the latest block height seen and a queue of events
        // awaiting a certain number of confirmations
        let mut latest_block;
        let mut pending: Vec<PendingEvent> = Vec::new();
        loop {
            // update the latest block height
            latest_block = loop {
                if let Ok(height) = oracle.eth_block_number().await {
                    break height;
                }
                if !oracle.connected() {
                    tracing::info!(
                        "Ethereum oracle could not send events to the ledger; \
                         the receiver has hung up. Shutting down"
                    );
                    return;
                }
            };
            // No blocks in existence yet with enough confirmations
            if Uint256::from(MIN_CONFIRMATIONS) > latest_block {
                if !oracle.connected() {
                    tracing::info!(
                        "Ethereum oracle could not send events to the ledger; \
                         the receiver has hung up. Shutting down"
                    );
                    return;
                }
                continue;
            }
            let block_to_check =
                latest_block.clone() - MIN_CONFIRMATIONS.into();
            // check for events with at least `[MIN_CONFIRMATIONS]`
            // confirmations.
            for sig in signatures::SIGNATURES {
                let addr: Address = match signatures::SigType::from(sig) {
                    signatures::SigType::Bridge => MINT_CONTRACT.0.into(),
                    signatures::SigType::Governance => {
                        GOVERNANCE_CONTRACT.0.into()
                    }
                };
                // fetch the events for matching the given signature
                let mut events = loop {
                    if let Ok(pending) = oracle
                        .check_for_events(
                            block_to_check.clone(),
                            Some(block_to_check.clone()),
                            vec![addr],
                            vec![sig],
                        )
                        .await
                        .map(|logs| {
                            logs.into_iter()
                                .filter_map(|log| {
                                    PendingEvent::decode(
                                        sig,
                                        block_to_check.clone(),
                                        log.data.0.as_slice(),
                                    )
                                    .ok()
                                })
                                .collect::<Vec<PendingEvent>>()
                        })
                    {
                        break pending;
                    }
                    if !oracle.connected() {
                        tracing::info!(
                            "Ethereum oracle could not send events to the \
                             ledger; the receiver has hung up. Shutting down"
                        );
                        return;
                    }
                };
                pending.append(&mut events);
                if !oracle.send(process_queue(&latest_block, &mut pending)) {
                    tracing::info!(
                        "Ethereum oracle could not send events to the ledger; \
                         the receiver has hung up. Shutting down"
                    );
                    return;
                }
            }
        }
    }

    /// Check which events in the queue have reached their
    /// required number of confirmations and remove them
    /// from the queue of pending events
    fn process_queue(
        latest_block: &Uint256,
        pending: &mut Vec<PendingEvent>,
    ) -> Vec<EthereumEvent> {
        let mut pending_tmp: Vec<PendingEvent> =
            Vec::with_capacity(pending.len());
        std::mem::swap(&mut pending_tmp, pending);
        let mut confirmed = vec![];
        for item in pending_tmp.into_iter() {
            if item.is_confirmed(latest_block) {
                confirmed.push(item.event);
            } else {
                pending.push(item);
            }
        }
        confirmed
    }

    #[cfg(test)]
    mod test_oracle {
        use namada::types::ethereum_events::TransferToEthereum;
        use tokio::sync::oneshot::{channel, Receiver};

        use super::*;
        use crate::node::ledger::ethereum_node::events::{
            ChangedContract, RawTransfersToEthereum,
        };
        use crate::node::ledger::ethereum_node::test_tools::mock_web3_client::{MockEventType, TestCmd, Web3};

        /// The data returned from setting up a test
        struct TestPackage {
            oracle: Oracle,
            admin_channel: tokio::sync::mpsc::UnboundedSender<TestCmd>,
            eth_recv: tokio::sync::mpsc::UnboundedReceiver<EthereumEvent>,
            abort_recv: Receiver<()>,
        }

        /// Set up an oracle with a mock web3 client that we can contr
        fn setup() -> TestPackage {
            let (admin_channel, client) = Web3::setup();
            let (eth_sender, eth_receiver) =
                tokio::sync::mpsc::unbounded_channel();
            let (abort, abort_recv) = channel();
            TestPackage {
                oracle: Oracle {
                    client,
                    sender: eth_sender,
                    abort: Some(abort),
                },
                admin_channel,
                eth_recv: eth_receiver,
                abort_recv,
            }
        }

        /// Test that if the oracle shuts down, it
        /// sends a message to the fullnode to stop
        #[test]
        fn test_abort_send() {
            let TestPackage {
                oracle,
                mut abort_recv,
                ..
            } = setup();
            drop(oracle);
            assert!(abort_recv.try_recv().is_ok())
        }

        /// Test that if the fullnode stops, the oracle
        /// shuts down, even if the web3 client is unresponsive
        #[test]
        fn test_shutdown() {
            let TestPackage {
                oracle,
                eth_recv,
                admin_channel,
                ..
            } = setup();
            let oracle = std::thread::spawn(move || {
                tokio_test::block_on(run_oracle_aux(oracle));
            });
            admin_channel
                .send(TestCmd::Unresponsive)
                .expect("Test failed");
            drop(eth_recv);
            oracle.join().expect("Test failed");
        }

        /// Test that if no logs are received from the web3
        /// client, no events are sent out
        #[test]
        fn test_no_logs_no_op() {
            let TestPackage {
                oracle,
                mut eth_recv,
                admin_channel,
                ..
            } = setup();
            let oracle = std::thread::spawn(move || {
                tokio_test::block_on(run_oracle_aux(oracle));
            });
            admin_channel
                .send(TestCmd::NewHeight(Uint256::from(100u32)))
                .expect("Test failed");
            let mut time = std::time::Duration::from_secs(1);
            while time > std::time::Duration::from_millis(10) {
                assert!(eth_recv.try_recv().is_err());
                time -= std::time::Duration::from_millis(10);
            }
            drop(eth_recv);
            oracle.join().expect("Test failed");
        }

        /// Test that if a new block height doesn't increase,
        /// no events are sent out even if there are
        /// some in the logs.
        #[test]
        fn test_cant_get_new_height() {
            let TestPackage {
                oracle,
                mut eth_recv,
                admin_channel,
                ..
            } = setup();
            let oracle = std::thread::spawn(move || {
                tokio_test::block_on(run_oracle_aux(oracle));
            });
            // Increase height above [`MIN_CONFIRMATIONS`]
            admin_channel
                .send(TestCmd::NewHeight(50u32.into()))
                .expect("Test failed");

            let new_event = ChangedContract {
                name: "Test".to_string(),
                address: EthAddress([0; 20]),
            }
            .encode();
            admin_channel
                .send(TestCmd::NewEvent {
                    event_type: MockEventType::NewContract,
                    data: new_event,
                    height: 51,
                })
                .expect("Test failed");
            // since height is not updating, we should not receive events
            let mut time = std::time::Duration::from_secs(1);
            while time > std::time::Duration::from_millis(10) {
                assert!(eth_recv.try_recv().is_err());
                time -= std::time::Duration::from_millis(10);
            }
            drop(eth_recv);
            oracle.join().expect("Test failed");
        }

        /// Test that the oracle waits until new logs
        /// are received before sending them on.
        #[test]
        fn test_wait_on_new_logs() {
            let TestPackage {
                oracle,
                mut eth_recv,
                admin_channel,
                ..
            } = setup();
            let oracle = std::thread::spawn(move || {
                tokio_test::block_on(run_oracle_aux(oracle));
            });
            // Increase height above [`MIN_CONFIRMATIONS`]
            admin_channel
                .send(TestCmd::NewHeight(50u32.into()))
                .expect("Test failed");

            let new_event = ChangedContract {
                name: "Test".to_string(),
                address: EthAddress([0; 20]),
            }
            .encode();
            admin_channel
                .send(TestCmd::NewEvent {
                    event_type: MockEventType::NewContract,
                    data: new_event,
                    height: 100,
                })
                .expect("Test failed");

            // we should not receive events even though the height is large
            // enough
            admin_channel
                .send(TestCmd::Unresponsive)
                .expect("Test failed");
            admin_channel
                .send(TestCmd::NewHeight(Uint256::from(101u32)))
                .expect("Test failed");

            let mut time = std::time::Duration::from_secs(1);
            while time > std::time::Duration::from_millis(10) {
                assert!(eth_recv.try_recv().is_err());
                time -= std::time::Duration::from_millis(10);
            }
            // check that when web3 becomes responsive, oracle sends event
            admin_channel.send(TestCmd::Normal).expect("Test failed");
            let event = eth_recv.blocking_recv().expect("Test failed");
            if let EthereumEvent::NewContract { name, address } = event {
                assert_eq!(name.as_str(), "Test");
                assert_eq!(address.0, [0; 20]);
            } else {
                panic!("Test failed");
            }
            drop(eth_recv);
            oracle.join().expect("Test failed");
        }

        /// Test that events are only sent when they
        /// reach the required number of confirmations
        #[test]
        fn test_finality_gadget() {
            let TestPackage {
                oracle,
                mut eth_recv,
                admin_channel,
                ..
            } = setup();
            let oracle = std::thread::spawn(move || {
                tokio_test::block_on(run_oracle_aux(oracle));
            });
            // Increase height above [`MIN_CONFIRMATIONS`]
            admin_channel
                .send(TestCmd::NewHeight(50u32.into()))
                .expect("Test failed");

            // confirmed after 50 blocks
            let first_event = ChangedContract {
                name: "Test".to_string(),
                address: EthAddress([0; 20]),
            }
            .encode();

            // confirmed after 75 blocks
            let second_event = RawTransfersToEthereum {
                transfers: vec![TransferToEthereum {
                    amount: Default::default(),
                    asset: EthAddress([0; 20]),
                    receiver: EthAddress([1; 20]),
                }],
                nonce: 1.into(),
                confirmations: 75,
            }
            .encode();

            // send in the events to the logs
            admin_channel
                .send(TestCmd::NewEvent {
                    event_type: MockEventType::TransferToEthereum,
                    data: second_event,
                    height: 125,
                })
                .expect("Test failed");
            admin_channel
                .send(TestCmd::NewEvent {
                    event_type: MockEventType::NewContract,
                    data: first_event,
                    height: 100,
                })
                .expect("Test failed");

            // increase block height so first event is confirmed but second is
            // not.
            admin_channel
                .send(TestCmd::NewHeight(Uint256::from(102u32)))
                .expect("Test failed");
            // check the correct event is received
            let event = eth_recv.blocking_recv().expect("Test failed");
            if let EthereumEvent::NewContract { name, address } = event {
                assert_eq!(name.as_str(), "Test");
                assert_eq!(address, EthAddress([0; 20]));
            } else {
                panic!("Test failed, {:?}", event);
            }

            // check no other events are received
            let mut time = std::time::Duration::from_secs(1);
            while time > std::time::Duration::from_millis(10) {
                assert!(eth_recv.try_recv().is_err());
                time -= std::time::Duration::from_millis(10);
            }

            // increase block height so second event is confirmed
            admin_channel
                .send(TestCmd::NewHeight(Uint256::from(130u32)))
                .expect("Test failed");
            // check correct event is received
            let event = eth_recv.blocking_recv().expect("Test failed");
            if let EthereumEvent::TransfersToEthereum {
                mut transfers, ..
            } = event
            {
                assert_eq!(transfers.len(), 1);
                let transfer = transfers.remove(0);
                assert_eq!(
                    transfer,
                    TransferToEthereum {
                        amount: Default::default(),
                        asset: EthAddress([0; 20]),
                        receiver: EthAddress([1; 20]),
                    }
                );
            } else {
                panic!("Test failed");
            }

            drop(eth_recv);
            oracle.join().expect("Test failed");
        }
    }
}

#[cfg(feature = "eth-fullnode")]
pub use oracle_process::*;
