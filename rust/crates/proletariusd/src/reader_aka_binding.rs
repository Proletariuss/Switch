//! Immutable PC/SC identity binding for async native authentication clients.
use crate::reader_task::ReaderTaskError;
use proletarius_ims::aka_exchange::SimAnswer;
use proletarius_reader::{AkaApplication, AuthenticationError};
use proletarius_sim::AkaResult;
use std::{sync::Arc, time::Duration};
use zeroize::Zeroizing;

#[derive(Debug)]
#[allow(dead_code)] // Installed by the native IMS owner in the next gated wiring stage.
pub(crate) enum ImsReaderError {
    Worker(ReaderTaskError),
    Card(AuthenticationError),
    Owner(crate::reader_pcsc::Failure),
}

#[derive(Debug)]
pub(crate) enum AuthenticationOperationError {
    Card(AuthenticationError),
    Owner(crate::reader_pcsc::Failure),
}

/// Bounded native-owner work shares the Rust reservation gate with the legacy
/// N-wait owner. Only Owner::with_transaction authorizes physical I/O, and a
/// timed-out waiter never releases a running blocking worker's admission.
pub(crate) async fn run_owned<F, T>(
    reader: &str,
    operation: F,
    deadline: Duration,
) -> Result<T, ReaderTaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    run_owned_with_gate(
        crate::reader_reservation::gate(),
        reader,
        operation,
        deadline,
        &SLOTS,
    )
    .await
}

async fn run_owned_with_gate<F, T>(
    gate: Arc<crate::reader_reservation::Gate>,
    reader: &str,
    operation: F,
    deadline: Duration,
    slots: &std::sync::OnceLock<Arc<tokio::sync::Semaphore>>,
) -> Result<T, ReaderTaskError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = slots
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| ReaderTaskError::Busy)?;
    crate::reader_task::run_with_gate(
        gate,
        reader,
        move || {
            let _slot = permit;
            operation()
        },
        deadline,
    )
    .await
}

#[allow(dead_code)] // Kept with authenticate_ims so late results remain zeroized before wiring.
fn ims_answer(result: Result<AkaResult, AuthenticationError>) -> Result<SimAnswer, ImsReaderError> {
    Ok(match result.map_err(ImsReaderError::Card)? {
        AkaResult::Success { res, ck, ik } => SimAnswer::Success {
            res: Zeroizing::new(res),
            ck: Zeroizing::new(ck),
            ik: Zeroizing::new(ik),
        },
        AkaResult::SynchronizationFailure { auts } => {
            SimAnswer::SynchronizationFailure(Zeroizing::new(auts))
        }
        AkaResult::AuthenticationReject => SimAnswer::Rejected,
    })
}

struct Binding {
    reader: String,
    application: AkaApplication,
    iccid: Zeroizing<String>,
    imsi: Zeroizing<String>,
}

/// Clones share an immutable identity, never a mutable "current reader" slot.
/// Deliberately no Debug/Serialize implementation for identifiers or secrets.
#[derive(Clone)]
pub(crate) struct ReaderAkaBinding(Arc<Binding>);

impl ReaderAkaBinding {
    pub(crate) fn new(
        reader: &str,
        application: AkaApplication,
        iccid: &str,
        imsi: &str,
    ) -> Result<Self, ()> {
        if reader.trim().is_empty()
            || reader.contains('\0')
            || !(18..=22).contains(&iccid.len())
            || !(5..=15).contains(&imsi.len())
            || !iccid
                .bytes()
                .chain(imsi.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return Err(());
        }
        Ok(Self(Arc::new(Binding {
            reader: reader.trim().into(),
            application,
            iccid: Zeroizing::new(iccid.into()),
            imsi: Zeroizing::new(imsi.into()),
        })))
    }

    pub(crate) async fn authenticate(
        &self,
        owner: crate::reader_pcsc::Owner,
        pcsc: Option<crate::reader_pcsc::Binding>,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> Result<Result<AkaResult, AuthenticationOperationError>, ReaderTaskError> {
        self.authenticate_with(
            rand,
            autn,
            move |reader, application, iccid, imsi, rand, autn| {
                owner
                    .with_transaction(pcsc.as_ref(), reader, |transport| {
                        proletarius_reader::authenticate_bound_with_transport(
                            transport,
                            application,
                            iccid,
                            imsi,
                            rand,
                            autn,
                        )
                    })
                    .map_err(AuthenticationOperationError::Owner)?
                    .map_err(AuthenticationOperationError::Card)
            },
        )
        .await
    }

    /// The result is wrapped inside the worker so even a late response after
    /// cancellation is dropped with secret buffers zeroized.
    #[allow(dead_code)] // Exercised once the native registration owner is enabled.
    pub(crate) async fn authenticate_ims(
        &self,
        owner: crate::reader_pcsc::Owner,
        pcsc: Option<crate::reader_pcsc::Binding>,
        rand: Zeroizing<[u8; 16]>,
        autn: Zeroizing<[u8; 16]>,
    ) -> Result<SimAnswer, ImsReaderError> {
        self.authenticate_with(
            *rand,
            *autn,
            move |reader, application, iccid, imsi, rand, autn| {
                owner
                    .with_transaction(pcsc.as_ref(), reader, |transport| {
                        ims_answer(proletarius_reader::authenticate_bound_with_transport(
                            transport,
                            application,
                            iccid,
                            imsi,
                            rand,
                            autn,
                        ))
                    })
                    .map_err(ImsReaderError::Owner)?
            },
        )
        .await
        .map_err(ImsReaderError::Worker)?
    }

    async fn authenticate_with<F, T>(
        &self,
        rand: [u8; 16],
        autn: [u8; 16],
        operation: F,
    ) -> Result<T, ReaderTaskError>
    where
        F: FnOnce(&str, AkaApplication, &str, &str, &[u8; 16], &[u8; 16]) -> T + Send + 'static,
        T: Send + 'static,
    {
        let binding = self.0.clone();
        let reader = binding.reader.clone();
        let rand = Zeroizing::new(rand);
        let autn = Zeroizing::new(autn);
        run_owned(
            &reader,
            move || {
                operation(
                    &binding.reader,
                    binding.application,
                    &binding.iccid,
                    &binding.imsi,
                    &rand,
                    &autn,
                )
            },
            Duration::from_secs(35),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_sim::ApduTransport;
    use std::collections::VecDeque;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn native_reader_workers_share_admission_with_multistandby_reservations() {
        let gate = Arc::new(crate::reader_reservation::Gate::default());
        let owner = gate
            .acquire(None, Some("reader-a"), Some(4))
            .expect("reservation should be granted while idle");
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_executed = executed.clone();
        assert_eq!(
            run_owned_with_gate(
                gate.clone(),
                "reader-a",
                move || worker_executed.store(true, Ordering::SeqCst),
                Duration::from_secs(1),
                &std::sync::OnceLock::new(),
            )
            .await,
            Err(ReaderTaskError::Busy)
        );
        assert!(!executed.load(Ordering::SeqCst));
        gate.release(&owner).unwrap();

        let (started, start) = tokio::sync::oneshot::channel();
        let (finish, wait) = std::sync::mpsc::channel();
        let (finished, done) = tokio::sync::oneshot::channel();
        let worker_gate = gate.clone();
        let worker_reader = "reader-a".to_owned();
        let worker = tokio::spawn(async move {
            run_owned_with_gate(
                worker_gate,
                &worker_reader,
                move || {
                    let _ = started.send(());
                    wait.recv().unwrap();
                    let _ = finished.send(());
                },
                Duration::from_millis(10),
                &std::sync::OnceLock::new(),
            )
            .await
        });
        start.await.unwrap();
        assert_eq!(worker.await.unwrap(), Err(ReaderTaskError::Timeout));
        assert_eq!(
            gate.acquire(None, Some("reader-a"), Some(5)),
            Err("card_busy")
        );
        finish.send(()).unwrap();
        done.await.unwrap();
        for _ in 0..100 {
            if let Ok(released) = gate.acquire(None, Some("reader-a"), Some(5)) {
                gate.release(&released).unwrap();
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("completed native worker must release reservation admission");
    }

    struct ScriptedWire {
        responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
        commands: Arc<Mutex<Vec<Vec<u8>>>>,
    }
    impl ApduTransport for ScriptedWire {
        type Error = crate::reader_pcsc::Failure;
        fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.commands.lock().unwrap().push(command.to_vec());
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted native APDU response"))
        }
    }
    struct ScriptedPhysical {
        wire: ScriptedWire,
        transactions: Arc<AtomicUsize>,
    }
    impl crate::reader_pcsc::Physical for ScriptedPhysical {
        fn transaction(
            &mut self,
            body: &mut dyn FnMut(&mut dyn ApduTransport<Error = crate::reader_pcsc::Failure>),
        ) -> Result<(), crate::reader_pcsc::Failure> {
            self.transactions.fetch_add(1, Ordering::SeqCst);
            body(&mut self.wire);
            Ok(())
        }
        fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
            Ok(())
        }
    }

    fn owner_fixture(
        responses: Vec<Vec<u8>>,
    ) -> (
        crate::reader_pcsc::Owner,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Vec<u8>>>>,
    ) {
        let opened = Arc::new(AtomicUsize::new(0));
        let transactions = Arc::new(AtomicUsize::new(0));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let count = opened.clone();
        let entered = transactions.clone();
        let recorded = commands.clone();
        let owner = crate::reader_pcsc::Owner::with_factory(
            "producer-fixture".into(),
            Arc::new(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(ScriptedPhysical {
                    wire: ScriptedWire {
                        responses: responses.clone(),
                        commands: recorded.clone(),
                    },
                    transactions: entered.clone(),
                }) as Box<dyn crate::reader_pcsc::Physical>)
            }),
            Arc::new(|| Ok(vec!["Reader A".into()])),
            Arc::new(|_, _| panic!("authentication must not reset the reader")),
        );
        (owner, opened, transactions, commands)
    }

    fn identity_and_aka_replies(include_aka: bool) -> Vec<Vec<u8>> {
        let ok = vec![0x90, 0];
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff];
        let record = [&[0x61, 10, 0x4f, 8][..], &aid, &ok].concat();
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut replies = vec![
            ok.clone(),
            ok.clone(),
            record.clone(),
            ok.clone(),
            ok.clone(),
            vec![8, 0x90, 0],
            [&imsi[..], &ok].concat(),
            ok.clone(),
            vec![0, 0, 0, 2, 0x90, 0],
            ok.clone(),
            ok.clone(),
            ok.clone(),
            [&iccid[..], &ok].concat(),
        ];
        if include_aka {
            let mut success = vec![0xdb, 8];
            success.extend_from_slice(&[1; 8]);
            success.push(16);
            success.extend_from_slice(&[2; 16]);
            success.push(16);
            success.extend_from_slice(&[3; 16]);
            success.extend_from_slice(&ok);
            replies.extend([ok.clone(), ok.clone(), record, ok, success]);
        }
        replies
    }

    #[tokio::test]
    async fn identity_verification_and_aka_use_one_native_physical_transaction() {
        for changed in [false, true] {
            let (owner, opened, transactions, commands) =
                owner_fixture(identity_and_aka_replies(!changed));
            let binding = ReaderAkaBinding::new(
                "Reader A",
                AkaApplication::Usim,
                if changed {
                    "8901234567890123457"
                } else {
                    "8901234567890123456"
                },
                "234100896392986",
            )
            .unwrap();
            let result = binding
                .authenticate(owner.clone(), None, [0x11; 16], [0x22; 16])
                .await
                .unwrap();
            if changed {
                assert!(matches!(
                    result,
                    Err(AuthenticationOperationError::Card(
                        AuthenticationError::IdentityChanged
                    ))
                ));
            } else {
                assert!(
                    matches!(result, Ok(AkaResult::Success { ck, ik, .. }) if ck == [2;16] && ik == [3;16])
                );
            }
            assert_eq!(opened.load(Ordering::SeqCst), 1);
            assert_eq!(transactions.load(Ordering::SeqCst), 1);
            assert_eq!(
                commands.lock().unwrap().len(),
                if changed { 13 } else { 18 }
            );
            assert_eq!(
                commands
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|command| command[1] == 0x88),
                !changed
            );
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn stale_supplied_native_lease_never_opens_a_replacement_reader() {
        let (owner, opened, _, commands) = owner_fixture(Vec::new());
        let binding = ReaderAkaBinding::new(
            "Reader A",
            AkaApplication::Usim,
            "8901234567890123456",
            "234100896392986",
        )
        .unwrap();
        let pcsc = crate::reader_pcsc::Binding {
            producer_id: "old-producer".into(),
            reader: "Reader A".into(),
            session_id: "old-session".into(),
            lease_id: "old-lease".into(),
            lease_seq: 1,
        };
        let result = binding
            .authenticate(owner.clone(), Some(pcsc), [1; 16], [2; 16])
            .await
            .unwrap();
        assert!(
            matches!(result, Err(AuthenticationOperationError::Owner(error)) if error.code == "reader_pcsc_stale_session")
        );
        assert_eq!(opened.load(Ordering::SeqCst), 0);
        assert!(commands.lock().unwrap().is_empty());
        owner.shutdown().await.unwrap();
    }

    struct BlockedPhysical {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
        closed: Arc<AtomicUsize>,
    }
    impl ApduTransport for BlockedPhysical {
        type Error = crate::reader_pcsc::Failure;
        fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.started.take().unwrap().send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(vec![0x90, 0])
        }
    }
    impl crate::reader_pcsc::Physical for BlockedPhysical {
        fn transaction(
            &mut self,
            body: &mut dyn FnMut(&mut dyn ApduTransport<Error = crate::reader_pcsc::Failure>),
        ) -> Result<(), crate::reader_pcsc::Failure> {
            body(self);
            Ok(())
        }
        fn close(&mut self) -> Result<(), crate::reader_pcsc::Failure> {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn timed_out_native_waiter_keeps_real_transaction_until_io_returns() {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let closed = Arc::new(AtomicUsize::new(0));
        let physical = Arc::new(Mutex::new(Some(BlockedPhysical {
            started: Some(started),
            release: wait,
            closed: closed.clone(),
        })));
        let owner = crate::reader_pcsc::Owner::with_factory(
            "producer-fixture".into(),
            Arc::new(move |_| {
                Ok(Box::new(physical.lock().unwrap().take().unwrap())
                    as Box<dyn crate::reader_pcsc::Physical>)
            }),
            Arc::new(|| Ok(vec!["Reader A".into()])),
            Arc::new(|_, _| panic!("authentication must not reset the reader")),
        );
        let task_owner = owner.clone();
        let waiter = tokio::spawn(run_owned(
            "Reader A",
            move || {
                task_owner.with_transaction(None, "Reader A", |transport| {
                    transport.transmit(&[0, 0xa4, 0, 0])
                })
            },
            Duration::from_millis(100),
        ));
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            waiter.await.unwrap(),
            Err(ReaderTaskError::Timeout)
        ));
        let competing = owner.clone();
        let denied = tokio::task::spawn_blocking(move || {
            competing.with_transaction(None, "Reader A", |_| ())
        })
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(denied.code, "reader_pcsc_busy");
        let shutdown_owner = owner.clone();
        let shutdown = tokio::spawn(async move { shutdown_owner.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn ims_challenge_uses_bound_identity_and_preserves_card_errors() {
        use proletarius_ims::{
            SipResponse,
            aka_authorization::AuthorizationRequest,
            aka_exchange::{ExchangeError, answer_challenge},
        };
        let binding = ReaderAkaBinding::new(
            "Reader A",
            AkaApplication::Usim,
            "8901234567890123456",
            "234100896392986",
        )
        .unwrap();
        let response = SipResponse::parse(format!("SIP/2.0 401 Challenge\r\nWWW-Authenticate: Digest realm=\"ims.test\",nonce=\"{}\",algorithm=AKAv1-MD5\r\nContent-Length: 0\r\n\r\n", "01".repeat(32)).as_bytes()).unwrap();
        let request = AuthorizationRequest {
            username: "user",
            method: "REGISTER",
            uri: "sip:ims.test",
            body: b"",
            cnonce: "c",
            nonce_count: 1,
        };
        let (_stop_tx, stop) = tokio::sync::watch::channel(false);
        for identity_changed in [false, true] {
            let binding = binding.clone();
            let result = answer_challenge(
                &response,
                &request,
                tokio::time::Instant::now() + Duration::from_secs(1),
                stop.clone(),
                |rand, autn| async move {
                    binding
                        .authenticate_with(
                            *rand,
                            *autn,
                            move |reader, _, iccid, imsi, rand, autn| {
                                assert_eq!(reader, "Reader A");
                                assert_eq!(iccid, "8901234567890123456");
                                assert_eq!(imsi, "234100896392986");
                                assert_eq!(rand, &[1; 16]);
                                assert_eq!(autn, &[1; 16]);
                                ims_answer(if identity_changed {
                                    Err(AuthenticationError::IdentityChanged)
                                } else {
                                    Ok(AkaResult::Success {
                                        res: vec![1; 8],
                                        ck: [2; 16],
                                        ik: [3; 16],
                                    })
                                })
                            },
                        )
                        .await
                        .map_err(ImsReaderError::Worker)?
                },
            )
            .await;
            if identity_changed {
                assert!(matches!(
                    result,
                    Err(ExchangeError::Sim(ImsReaderError::Card(
                        AuthenticationError::IdentityChanged
                    )))
                ));
            } else {
                let result = result.unwrap();
                assert_eq!(result.header().name(), "Authorization");
                assert_eq!(*result.security_keys().unwrap().ik, [3; 16]);
            }
        }
    }
    #[tokio::test]
    async fn independent_bindings_keep_reader_identity_and_application() {
        let a = ReaderAkaBinding::new(
            " Reader A ",
            AkaApplication::Usim,
            "8901234567890123456",
            "234100896392986",
        )
        .unwrap();
        let b = ReaderAkaBinding::new(
            "Reader B",
            AkaApplication::Isim,
            "8901234567890123457",
            "234100896392987",
        )
        .unwrap();
        let a_clone = a.clone();
        drop(a);
        let one = a_clone.authenticate_with([1; 16], [2; 16], |r, app, iccid, imsi, rand, autn| {
            assert_eq!(r, "Reader A");
            assert_eq!(app, AkaApplication::Usim);
            assert_eq!(iccid, "8901234567890123456");
            assert_eq!(imsi, "234100896392986");
            assert_eq!(rand, &[1; 16]);
            assert_eq!(autn, &[2; 16]);
            Ok::<_, AuthenticationError>(AkaResult::AuthenticationReject)
        });
        let two = b.authenticate_with([3; 16], [4; 16], |r, app, iccid, imsi, rand, autn| {
            assert_eq!(r, "Reader B");
            assert_eq!(app, AkaApplication::Isim);
            assert_eq!(iccid, "8901234567890123457");
            assert_eq!(imsi, "234100896392987");
            assert_eq!(rand, &[3; 16]);
            assert_eq!(autn, &[4; 16]);
            Err::<AkaResult, _>(AuthenticationError::IdentityChanged)
        });
        let (one, two) = tokio::join!(one, two);
        assert!(matches!(one, Ok(Ok(AkaResult::AuthenticationReject))));
        assert!(matches!(two, Ok(Err(AuthenticationError::IdentityChanged))));
        assert!(
            ReaderAkaBinding::new(
                "reader\0",
                AkaApplication::Usim,
                "8901234567890123456",
                "234100896392986"
            )
            .is_err()
        );
    }
}
