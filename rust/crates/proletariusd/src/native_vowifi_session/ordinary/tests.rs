use super::*;
use crate::{reader_pcsc, reader_reservation, reader_session_lease};

struct NoIo;
impl proletarius_sim::ApduTransport for NoIo {
    type Error = reader_pcsc::Failure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        panic!("stopped startup must not issue AKA/APDU")
    }
}
impl reader_pcsc::Physical for NoIo {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn proletarius_sim::ApduTransport<Error = reader_pcsc::Failure>),
    ) -> Result<(), reader_pcsc::Failure> {
        body(self);
        Ok(())
    }
    fn close(&mut self) -> Result<(), reader_pcsc::Failure> {
        Ok(())
    }
}
struct Supplier {
    session: reader_pcsc::LifecycleBinding,
}
impl reader_session_lease::NativeSessionSupplier for Supplier {
    fn pre_admit(
        self: Arc<Self>,
        _: String,
        _: reader_reservation::NativePinPolicy,
        _ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> reader_session_lease::PreAdmissionFuture {
        Box::pin(async { Ok(4) })
    }
    fn admit(
        self: Arc<Self>,
        _: reader_reservation::NativePinBinding,
    ) -> reader_session_lease::AdmissionFuture {
        Box::pin(async { Ok(()) })
    }
    fn snapshot(
        &self,
        b: &reader_reservation::NativePinBinding,
    ) -> Result<serde_json::Value, String> {
        Ok(
            json!({"reader":b.reader,"reader_generation":b.reader_generation,"session":self.session,
        "identity":{"iccid":"894433000000000001","imsi":"234330000000001","mcc":"234","mnc":"33",
        "private_identity":"234330000000001@ims.mnc033.mcc234.3gppnetwork.org", "public_identity":"sip:234330000000001@ims.mnc033.mcc234.3gppnetwork.org"}}),
        )
    }
    fn finalize(
        self: Arc<Self>,
        _: reader_reservation::NativePinBinding,
        _: Option<reader_session_lease::NativeCardContext>,
        _: String,
        _: String,
    ) -> reader_session_lease::SupplierFuture {
        Box::pin(async { Err("no mutation to finalize".into()) })
    }
}

async fn startup_release_fixture(stop_before_start: bool, hold_release: bool, guard_panic: bool) {
    let reader = "ordinary-cancel-fixture";
    let physical = reader_pcsc::Owner::with_factory(
        "ordinary-fixture-producer-0000001".into(),
        Arc::new(|_| Ok(Box::new(NoIo))),
        Arc::new(move || Ok(vec![reader.into()])),
        Arc::new(|_, _| panic!("must not reset card")),
    );
    let session = physical.native_open(reader).unwrap();
    let gate = Arc::new(reader_reservation::Gate::default());
    gate.initialize_business_producer("old-fixture");
    gate.initialize_native_reader_producer("ordinary-fixture-producer-0000001");
    gate.test_seed_selection(reader, 4);
    let supplier: Arc<dyn reader_session_lease::NativeSessionSupplier> =
        Arc::new(Supplier { session });
    gate.set_native_session_supplier(Arc::downgrade(&supplier));
    let lease = reader_session_lease::NativeReaderSessionLease::acquire(
        Path::new("unused.sock"),
        None,
        reader,
        gate.clone(),
        physical.clone(),
    )
    .await
    .unwrap();
    // Freeze production carrier/profile types without invoking host proxy/DNS
    // discovery; this test stops before IKE and deliberately has no network IO.
    let sim = SimIdentity {
        mcc: "234".into(),
        mnc: "33".into(),
        imsi: lease.identity.imsi.to_string(),
        iccid: lease.identity.iccid.to_string(),
        ..Default::default()
    };
    let carrier = Registry::embedded()
        .unwrap()
        .resolve(&sim)
        .unwrap()
        .unwrap();
    let (config, runtime) = carrier
        .native_session_tunnel_config(&sim, None, "epdg.fixture.invalid", Some(4500))
        .unwrap();
    let ims = carrier
        .native_ims_register_profile_with_home_domain(&sim, None, None, false)
        .unwrap();
    let subscriber_identity =
        super::super::resolve_session_subscriber_identity(&ims, &lease.identity).unwrap();
    let device_identity = ims
        .resolve_device_identity(Some("356938035643809"))
        .unwrap();
    let context = NativeSessionContext {
        reader: reader.into(),
        reader_generation: 4,
        session_key: crate::native_vowifi_supervisor::ReaderSessionKey::new(
            reader,
            &lease.identity.iccid,
        )
        .unwrap(),
        sms_subscription_id: super::super::native_device_sms_subscription_id(
            &lease.identity.iccid,
            &lease.identity.imsi,
        ),
        mcc: sim.mcc,
        mnc: sim.mnc,
        carrier_id: carrier.id,
        config,
        runtime,
        access_isim: false,
        ims,
        subscriber_identity,
        device_identity,
        ike_device_identity_imei: None,
        configuration_status: json!({}),
        transport: SelectedTransport {
            mode: proletarius_vowifi::ProxyMode::Socks5UdpDomain,
            proxy: Some("127.0.0.1:9".parse().unwrap()),
            proxy_error: None,
            proxy_source: None,
        },
        card: SessionCard::leased(Arc::new(lease)),
    };
    let key = context.session_key.clone();
    let worker_completion = if hold_release {
        context
            .card
            .ordinary_lease()
            .unwrap()
            .set_release_timeout(Duration::from_millis(10));
        let lease = context.card.ordinary_lease().unwrap().clone();
        let card = lease.pcsc_context(physical.clone()).unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (complete, finish) = std::sync::mpsc::channel();
        let (ended, done) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            lease
                .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
                    let result = card.with_transaction(|_| {
                        started.send(()).unwrap();
                        finish.recv_timeout(Duration::from_secs(5)).unwrap();
                    });
                    ended.send(result.is_ok()).unwrap();
                })
                .await
        });
        ready.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        Some((complete, done))
    } else {
        None
    };
    let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
        .join("tmp/ordinary-cancel-fixture");
    let inputs = Inputs {
        sms_archive: Default::default(),
        reader_gate: gate.clone(),
        settings: NativeRegisteredStartupSettings {
            endpoints: NativeSecurityEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5064,
                port_s: 5063,
            },
            deadline: tokio::time::Instant::now() + Duration::from_secs(1),
            probe_interval: Duration::from_secs(30),
            retry_interval: Duration::from_secs(1),
            attempts: 1,
        },
        data_root: root.clone(),
        transactions: Default::default(),
        call_facts: crate::call_facts::CallFacts::new(
            proletarius_store::ArchiveStore::new(root.join("archive.json")),
            "ordinary-test".into(),
        ),
    };
    let mut owner = NativeSessionServiceOwner::new(if hold_release {
        Duration::from_millis(10)
    } else {
        Duration::from_secs(60)
    })
    .unwrap();
    let mut handle = owner
        .launch_ordinary(context, inputs)
        .unwrap_or_else(|_| panic!("admission"));
    let unrelated = guard_panic.then(|| {
        owner
            .reserve_session(
                &crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "unrelated-ordinary-reader",
                    "894433000000000002",
                )
                .unwrap(),
            )
            .ok()
            .unwrap()
    });
    assert_eq!(*handle.phase.borrow(), Phase::Starting);
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.binding.reader, reader);
    assert_eq!(snapshot.binding.reader_generation, 4);
    assert_eq!(snapshot.binding.session_generation, handle.generation);
    assert!(!snapshot.sms_ready && !snapshot.call_ready && !snapshot.lease_released);
    let cloned = handle.clone();
    assert_eq!(cloned.binding(), handle.binding());
    drop(cloned); // A status/control waiter never owns or cancels startup.
    assert!(owner.supervisor.lock().unwrap().snapshot(&key).is_some());
    if stop_before_start {
        handle.request_stop();
        owner.stop_generation(handle.generation);
    }
    if guard_panic {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = super::super::service::DriverStopGuard(handle.clone());
            panic!("submitted driver failed before handle publication");
        }));
        assert!(panic.is_err());
        assert!(!*unrelated.as_ref().unwrap().stop_signal().borrow());
    }
    if let Some((complete, done)) = worker_completion {
        tokio::time::timeout(Duration::from_secs(2), async {
            while handle.snapshot().phase != Phase::CleanupUnconfirmed {
                handle.status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(owner.supervisor.lock().unwrap().snapshot(&key).is_some());
        assert!(gate.native_reader_session_active());
        assert!(!handle.snapshot().lease_released && !handle.snapshot().may_restart());
        assert_eq!(
            handle.snapshot().cleanup_error,
            Some("native_vowifi_cleanup_unconfirmed")
        );
        assert!(
            handle
                .snapshot()
                .cleanup_detail()
                .unwrap()
                .contains("release remains unconfirmed")
        );
        assert!(!handle.snapshot().sms_ready && !handle.snapshot().call_ready);
        complete.send(()).unwrap();
        assert!(done.await.unwrap());
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while *handle.phase.borrow() != Phase::Released {
            handle.phase.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    owner.reap_finished_session_tasks().await;
    assert!(owner.ordinary_tasks.is_empty());
    let (_, result) = owner.ordinary_results.back().unwrap();
    match &result.outcome {
        Outcome::Startup {
            error: NativeRegisteredStartupFailure::Tunnel(error),
            ..
        } => {
            if stop_before_start || guard_panic {
                assert!(matches!(error, NativeTunnelStartError::Stopped));
            } else {
                assert!(matches!(error, NativeTunnelStartError::Transport(_)));
            }
        }
        _ => panic!("startup result lost"),
    }
    if hold_release {
        assert!(
            matches!(result.lease_release, Some(Err(_))),
            "the initial timeout cannot be rewritten as a successful release"
        );
    } else {
        assert!(matches!(result.lease_release, Some(Ok(()))));
    }
    assert_eq!(
        result.termination.reason,
        if stop_before_start {
            EndReason::UserStop
        } else {
            EndReason::StartupFailure
        }
    );
    assert_eq!(
        result.termination.retry,
        if stop_before_start {
            RetryClass::Stop
        } else if guard_panic {
            RetryClass::Stop
        } else {
            RetryClass::RetryAfterRelease
        }
    );
    let snapshot = handle.snapshot();
    assert_eq!(snapshot.phase, Phase::Released);
    assert!(snapshot.cleanup_confirmed && snapshot.lease_released);
    assert!(!snapshot.sms_ready && !snapshot.call_ready);
    assert_eq!(snapshot.may_restart(), !stop_before_start && !guard_panic);
    assert!(snapshot.termination.unwrap().detail().is_some());
    if let Some(unrelated) = unrelated {
        assert!(!*unrelated.stop_signal().borrow());
        owner.supervisor.lock().unwrap().worker_finished(&unrelated);
    }
    assert!(owner.supervisor.lock().unwrap().is_empty());
    assert!(handle.controls.is_closed());
    let replacement = reader_session_lease::NativeReaderSessionLease::acquire(
        Path::new("unused.sock"),
        None,
        reader,
        gate,
        physical,
    )
    .await
    .unwrap();
    replacement.release_with_confirmation().await.unwrap();
    assert!(owner.finish_after_sessions().await.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn stopped_startup_releases_the_actual_native_lease_without_network_or_card_io() {
    startup_release_fixture(true, false, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn failed_startup_releases_the_actual_native_lease_without_public_network_or_card_io() {
    startup_release_fixture(false, false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_physical_worker_retains_cleanup_until_original_lease_release_is_confirmed() {
    startup_release_fixture(true, true, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn driver_publication_panic_stops_only_its_captured_generation_without_inventing_user_stop() {
    startup_release_fixture(false, false, true).await;
}

#[tokio::test]
async fn actual_registered_renewal_preserves_ready_state_until_live_dialog_hangup() {
    let observed: Arc<Mutex<Option<(Reporter, watch::Receiver<Snapshot>)>>> =
        Arc::new(Mutex::new(None));
    let registered = observed.clone();
    let renewal = observed.clone();
    let ready = observed.clone();
    crate::native_registered_session::tests::renewal_preserves_live_call_with_hooks(
        move |session| {
            // The shared fixture has actually completed REGISTER and attached
            // the production SMS/call consumers, using this userspace address.
            let (reporter, status) = Reporter::new(Binding {
                reader: "combined-reader".into(),
                reader_generation: 4,
                session_generation: SessionGeneration::from_value(1).unwrap(),
                key: crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "combined-reader",
                    "8901234567890123456",
                )
                .unwrap(),
            });
            reporter.tunnel_started();
            reporter.tunnel_ready(&proletarius_vowifi::TunnelConfiguration {
                ipv4_addresses: vec!["10.0.0.2".parse().unwrap()],
                ..Default::default()
            });
            reporter.register_started();
            reporter.registered(session);
            reporter.consumers_ready("+447802002606", "sip:smsc@ims.example");
            assert_eq!(status.borrow().phase, Phase::Registered);
            *registered.lock().unwrap() = Some((reporter, status));
        },
        move |notice| {
            assert!(matches!(
                notice,
                registered::Notice::Sms(
                    crate::native_sms_consumer::SessionOutcome::RegistrationRenewalDue { .. }
                )
            ));
            let observed = renewal.lock().unwrap();
            observed.as_ref().unwrap().0.renewal_due();
        },
        move || {
            let observed = ready.lock().unwrap();
            let snapshot = observed.as_ref().unwrap().1.borrow();
            snapshot.renewal_due
                && snapshot.phase == Phase::Registered
                && snapshot.sms_ready
                && snapshot.call_ready
        },
    )
    .await;
    let observed = observed.lock().unwrap();
    let (reporter, status) = observed.as_ref().unwrap();
    // Production dispatcher/actor really drained the live call (including
    // actual peer BYE). The ordinary owner can now close admission.
    reporter.draining();
    assert_eq!(status.borrow().phase, Phase::Draining);
    assert!(!status.borrow().sms_ready && !status.borrow().call_ready);
    assert!(!status.borrow().lease_released && !status.borrow().may_restart());
}

fn policy_exit(
    actor_reason: native_ims_actor::StopReason,
    consumer_reason: registered::StopReason,
    network: Result<(), NativeNetworkError<Infallible>>,
) -> NativeRegisteredWorkerExit {
    let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
    let permit = supervisor
        .reserve(
            crate::native_vowifi_supervisor::ReaderSessionKey::new(
                "termination-fixture",
                "894433000000000001",
            )
            .unwrap(),
        )
        .unwrap();
    NativeRegisteredWorkerExit {
        lease_lost: false,
        local_retired: true,
        consumer: Ok(registered::Exit {
            generation: permit.generation(),
            reason: consumer_reason,
            undelivered_notice: None,
            timer_failures: Vec::new(),
            pending_reports: Vec::new(),
            sms: Vec::new(),
            media_cleanup: Ok(()),
        }),
        actor: Ok(native_ims_actor::Exit {
            undelivered: Vec::new(),
            permit,
            reason: actor_reason,
            cleanup: Ok(()),
        }),
        network: Ok(network),
        call_archive: Ok(()),
        call_archive_retired: true,
    }
}

#[test]
fn renewal_and_planned_reauth_are_not_misclassified_as_user_stop_by_cleanup_cancel() {
    let renewal = Outcome::Running(Ok(policy_exit(
        native_ims_actor::StopReason::Requested,
        registered::StopReason::RenewalDue,
        Ok(()),
    )));
    let termination = classify_outcome(&renewal, false);
    assert_eq!(termination.reason, EndReason::RegistrationRenewal);
    assert_eq!(termination.retry, RetryClass::RetryAfterRelease);
    assert_eq!(termination.public_error, None);
    let planned = Outcome::Running(Ok(policy_exit(
        native_ims_actor::StopReason::Requested,
        registered::StopReason::Requested,
        Err(NativeNetworkError::Tunnel(
            proletarius_vowifi::IkeAuthError::PlannedReauthentication,
        )),
    )));
    assert_eq!(
        classify_outcome(&planned, false).reason,
        EndReason::PlannedReauthentication
    );
    assert_eq!(classify_outcome(&planned, true).reason, EndReason::UserStop);
    assert_eq!(classify_outcome(&planned, true).retry, RetryClass::Stop);
}

#[test]
fn captured_lease_loss_stops_retry_while_remote_and_network_loss_allow_only_released_retry() {
    let network = Outcome::Running(Ok(policy_exit(
        native_ims_actor::StopReason::Requested,
        registered::StopReason::Requested,
        Err(NativeNetworkError::Network(
            proletarius_vowifi::netstack_runtime::NetworkRunError::SendTimeout,
        )),
    )));
    let termination = classify_outcome(&network, false);
    assert_eq!(termination.reason, EndReason::NetworkFailure);
    assert_eq!(termination.retry, RetryClass::RetryAfterRelease);
    assert!(termination.detail().unwrap().contains("SendTimeout"));
    let lost = Outcome::Running(Ok(policy_exit(
        native_ims_actor::StopReason::LeaseLost,
        registered::StopReason::Requested,
        Err(NativeNetworkError::Network(
            proletarius_vowifi::netstack_runtime::NetworkRunError::TunnelClosed,
        )),
    )));
    assert_eq!(
        classify_outcome(&lost, false).reason,
        EndReason::ReaderChanged
    );
    assert_eq!(classify_outcome(&lost, false).retry, RetryClass::Stop);
    let remote = Outcome::Running(Ok(policy_exit(
        native_ims_actor::StopReason::StreamClosed,
        registered::StopReason::ActorClosed,
        Ok(()),
    )));
    assert_eq!(
        classify_outcome(&remote, false).reason,
        EndReason::RemoteFailure
    );
    let unclaimed = Outcome::Running(Ok(policy_exit(
        native_ims_actor::StopReason::Requested,
        registered::StopReason::Requested,
        Ok(()),
    )));
    assert_eq!(
        classify_outcome(&unclaimed, false).reason,
        EndReason::InternalFailure
    );
    assert_ne!(
        classify_outcome(&unclaimed, false).reason,
        EndReason::UserStop
    );
}

#[test]
fn transient_startup_timeout_and_permanent_certificate_policy_have_distinct_retry_classes() {
    assert_eq!(
        startup_retry(&NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::Startup(TunnelStartupError::Deadline)
        )),
        RetryClass::RetryAfterRelease
    );
    assert_eq!(
        startup_retry(&NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::CertificateValidationUnavailable
        )),
        RetryClass::Permanent
    );
    assert_eq!(
        startup_retry(&NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::Stopped
        )),
        RetryClass::Stop
    );
}

#[test]
fn safe_startup_diagnostic_does_not_change_retry_error_or_cleanup_facts() {
    let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
    let permit = supervisor
        .reserve(
            crate::native_vowifi_supervisor::ReaderSessionKey::new(
                "diagnostic-reader",
                "894433000000000001",
            )
            .unwrap(),
        )
        .unwrap();
    let outcome = Outcome::Startup {
        permit,
        error: NativeRegisteredStartupFailure::Tunnel(NativeTunnelStartError::Startup(
            TunnelStartupError::Deadline,
        )),
    };
    let termination = classify_outcome(&outcome, false);
    assert_eq!(termination.reason, EndReason::StartupFailure);
    assert_eq!(termination.retry, RetryClass::RetryAfterRelease);
    assert_eq!(
        termination.public_error,
        Some("native_vowifi_startup_failed")
    );
    assert_eq!(termination.startup_failure.unwrap().stage, "ike_startup");
    let stopped = classify_outcome(&outcome, true);
    assert_eq!(stopped.reason, EndReason::UserStop);
    assert_eq!(stopped.retry, RetryClass::Stop);
    assert!(stopped.public_error.is_none());
    assert!(stopped.startup_failure.is_none());
}

#[tokio::test]
async fn joined_true_or_closed_lease_loss_beats_simultaneous_planned_network_exit() {
    for (physical_loss, closed) in [(true, false), (false, false), (false, true)] {
        let prepared = policy_exit(
            native_ims_actor::StopReason::Requested,
            registered::StopReason::Requested,
            Err(NativeNetworkError::Tunnel(
                proletarius_vowifi::IkeAuthError::PlannedReauthentication,
            )),
        );
        let actor_exit = prepared.actor.unwrap();
        let consumer_exit = prepared.consumer.unwrap();
        let cancel = actor_exit.permit.cancellation_sender();
        let mut actor_stop = cancel.subscribe();
        let mut consumer_stop = cancel.subscribe();
        let (loss_sender, loss) = watch::channel(physical_loss);
        // Keep the false producer live in the healthy case. A closed producer
        // here is unexpected: the retained heartbeat has not been stopped.
        let loss_sender = if closed {
            drop(loss_sender);
            None
        } else {
            Some(loss_sender)
        };
        let actor = tokio::spawn(async move {
            while !*actor_stop.borrow() {
                actor_stop.changed().await.unwrap();
            }
            actor_exit
        });
        let consumer = tokio::spawn(async move {
            while !*consumer_stop.borrow() {
                consumer_stop.changed().await.unwrap();
            }
            consumer_exit
        });
        let network = tokio::spawn(async {
            Err(NativeNetworkError::Tunnel(
                proletarius_vowifi::IkeAuthError::PlannedReauthentication,
            ))
        });
        let (ike_stop, _) = watch::channel(false);
        let (wake, _) = watch::channel(0);
        let (_, _, runtime) = proletarius_vowifi::ims_network_runtime::ImsNetworkRuntime::new(wake);
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/499-joined-loss-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let facts = crate::call_facts::NativeHistory::start_reader_ims(
            crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("calls.json")),
                "test".into(),
            ),
            "native-loss".into(),
            8,
        );
        // Network completion drives real parent cancellation before either
        // consumer samples the loss watch. Both actual joined exits say Requested.
        let exit = tokio::time::timeout(
            Duration::from_secs(3),
            join_registered_consumers(
                actor,
                consumer,
                network,
                cancel,
                ike_stop,
                facts,
                runtime.retirement(),
                loss,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            exit.actor.as_ref().unwrap().reason,
            native_ims_actor::StopReason::Requested
        );
        assert_eq!(
            exit.consumer.as_ref().unwrap().reason,
            registered::StopReason::Requested
        );
        assert!(exit.call_archive_retired && exit.call_archive.is_ok());
        assert_eq!(exit.lease_lost, physical_loss || closed);
        let outcome = Outcome::Running(Ok(exit));
        let termination = classify_outcome(&outcome, false);
        assert_eq!(
            termination.reason,
            if physical_loss || closed {
                EndReason::ReaderChanged
            } else {
                EndReason::PlannedReauthentication
            }
        );
        assert_eq!(
            termination.retry,
            if physical_loss || closed {
                RetryClass::Stop
            } else {
                RetryClass::RetryAfterRelease
            }
        );
        assert_eq!(classify_outcome(&outcome, true).reason, EndReason::UserStop);
        drop(loss_sender);
    }
}

#[test]
fn startup_datagram_retry_excludes_proxy_auth_config_and_protocol_failures() {
    use proletarius_vowifi::{RelayError, SocksCodecError};
    for error in [
        DatagramTransportError::MissingProxy,
        DatagramTransportError::InvalidPort,
        DatagramTransportError::Relay(RelayError::Codec(SocksCodecError::AuthenticationRejected(
            0xff,
        ))),
        DatagramTransportError::Relay(RelayError::Codec(SocksCodecError::AssociateRejected(2))),
        DatagramTransportError::Relay(RelayError::Codec(SocksCodecError::AssociateRejected(7))),
        DatagramTransportError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        DatagramTransportError::Io(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
    ] {
        assert_eq!(
            startup_datagram_retry(&error),
            RetryClass::Permanent,
            "{error:?}"
        );
    }
    for error in [
        DatagramTransportError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
        DatagramTransportError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        DatagramTransportError::Relay(RelayError::ControlClosed),
        DatagramTransportError::Relay(RelayError::AssociateTimeout),
        DatagramTransportError::Relay(RelayError::Codec(SocksCodecError::AssociateRejected(3))),
    ] {
        assert_eq!(
            startup_datagram_retry(&error),
            RetryClass::RetryAfterRelease,
            "{error:?}"
        );
    }
}

#[test]
fn register_retry_uses_typed_temporary_status_and_never_authentication_or_template_errors() {
    use proletarius_ims::{SipError, registration_io::RegisterWaitError};
    use proletarius_vowifi::registration_coordinator::{RegistrationError, RegistrationStage};
    let stage = RegistrationStage::InitialRegister;
    for status in [401, 403, 407, 404, 488] {
        let error: RegistrationError<()> = RegistrationError::Transport {
            stage,
            source: RegisterWaitError::Sip(SipError::RegistrationRejected(status)),
        };
        assert_eq!(
            startup_registration_retry(&error),
            RetryClass::Permanent,
            "{status}"
        );
    }
    for status in [408, 500, 502, 503, 504] {
        let error: RegistrationError<()> = RegistrationError::Transport {
            stage,
            source: RegisterWaitError::Sip(SipError::RegistrationRejected(status)),
        };
        assert_eq!(
            startup_registration_retry(&error),
            RetryClass::RetryAfterRelease,
            "{status}"
        );
    }
    let authentication = RegistrationError::Authentication(());
    assert_eq!(
        startup_registration_retry(&authentication),
        RetryClass::Permanent
    );
    let template: RegistrationError<()> = RegistrationError::InvalidPlan("template_invalid");
    assert_eq!(startup_registration_retry(&template), RetryClass::Permanent);
    let invalid_sip: RegistrationError<()> = RegistrationError::Transport {
        stage,
        source: RegisterWaitError::Sip(SipError::InvalidHeader),
    };
    assert_eq!(
        startup_registration_retry(&invalid_sip),
        RetryClass::Permanent
    );
    let disconnected: RegistrationError<()> = RegistrationError::Transport {
        stage,
        source: RegisterWaitError::Disconnected,
    };
    assert_eq!(
        startup_registration_retry(&disconnected),
        RetryClass::RetryAfterRelease
    );
    let cancelled: RegistrationError<()> = RegistrationError::Cancelled(stage);
    assert_eq!(startup_registration_retry(&cancelled), RetryClass::Stop);
}
