use super::*;

#[test]
fn ike_phases_use_only_actual_typed_evidence() {
    let cases = [
        (TunnelStartupError::Deadline, "ike_startup", "timeout"),
        (
            TunnelStartupError::SaInit(SaInitError::Timeout),
            "ike_sa_init",
            "timeout",
        ),
        (
            TunnelStartupError::SaInit(SaInitError::ProposalRejected),
            "ike_sa_init",
            "proposal_rejected",
        ),
        (
            TunnelStartupError::Eap(EapHandshakeError::Rejected),
            "ike_eap_aka",
            "authentication_rejected",
        ),
        (
            TunnelStartupError::Eap(EapHandshakeError::Deadline),
            "ike_eap_aka",
            "timeout",
        ),
        (
            TunnelStartupError::Auth(IkeAuthError::PeerNotify(24)),
            "ike_auth",
            "peer_rejected",
        ),
        (
            TunnelStartupError::Auth(IkeAuthError::ResponderAuthenticationFailed),
            "ike_auth",
            "responder_authentication_failed",
        ),
        (
            TunnelStartupError::Auth(IkeAuthError::Configuration(
                TunnelConfigError::MissingAddress,
            )),
            "ike_auth",
            "internal_address_missing",
        ),
    ];
    for (error, stage, code) in cases {
        let value = startup(&NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::Startup(error),
        ));
        assert_eq!((value.stage, value.code), (stage, code));
        assert!(!value.message.is_empty());
    }
}

#[test]
fn eap_failure_kinds_are_specific_without_serializing_private_card_errors() {
    let marker = "IMSI-SECRET RAND-SECRET AUTN-SECRET";
    let cases = [
        (
            EapHandshakeError::Authentication(EapAkaExchangeError::SimService(
                NativeAkaError::Lease(proletarius_domain::BackendFailure::Io(marker.into())),
            )),
            "sim_authentication_unavailable",
        ),
        (
            EapHandshakeError::Protocol(Ikev2EapError::Aka(EapAkaError::MacMismatch)),
            "eap_mac_mismatch",
        ),
        (
            EapHandshakeError::Protocol(Ikev2EapError::InvalidPacket),
            "eap_packet_invalid",
        ),
        (
            EapHandshakeError::Transport(IkeAuthError::ResponderAuthenticationFailed),
            "responder_authentication_failed",
        ),
    ];
    for (error, code) in cases {
        let value = startup(&NativeRegisteredStartupFailure::Tunnel(
            NativeTunnelStartError::Startup(TunnelStartupError::Eap(error)),
        ));
        assert_eq!(value.stage, "ike_eap_aka");
        assert_eq!(value.code, code);
        let public = serde_json::to_string(&value).unwrap();
        assert!(!public.contains(marker));
    }
    let unsupported = startup(&NativeRegisteredStartupFailure::Tunnel(
        NativeTunnelStartError::Startup(TunnelStartupError::Eap(EapHandshakeError::Protocol(
            Ikev2EapError::UnsupportedAkaSubtype(5),
        ))),
    ));
    assert_eq!(unsupported.eap_subtype, Some(5));
}
#[test]
fn ims_forbidden_and_security_requirements_preserve_stage() {
    let forbidden: RegistrationError<()> = RegistrationError::Transport {
        stage: RegistrationStage::ProtectedRegister,
        source: RegisterWaitError::Sip(SipError::RegistrationRejected(403)),
    };
    let value = registration(&forbidden);
    assert_eq!(
        (value.stage, value.code, value.sip_status),
        (
            "ims_protected_register",
            "registration_forbidden",
            Some(403)
        )
    );
    let security: RegistrationError<()> = RegistrationError::SecurityPreparation(
        proletarius_ims::registration_connection::SecAgreePrepareError::MissingSecurityServer,
    );
    assert_eq!(registration(&security).stage, "ims_security");
    assert_eq!(registration(&security).code, "security_offer_invalid");
    let unavailable: RegistrationError<()> = RegistrationError::Transport {
        stage: RegistrationStage::ProtectedConnect,
        source: RegisterWaitError::SecurityTransportUnavailable,
    };
    assert_eq!(
        registration(&unavailable).code,
        "security_transport_unavailable"
    );
}
#[test]
fn private_io_and_aka_material_never_enter_public_diagnostic() {
    let marker = "IMSI-SECRET RAND-SECRET AUTN-SECRET Authorization-SECRET";
    let errors = [
        RegistrationError::Io {
            stage: RegistrationStage::ProtectedConnect,
            source: std::io::Error::other(marker),
        },
        RegistrationError::Authentication(marker),
        RegistrationError::InvalidPlan(marker),
        RegistrationError::Sip {
            stage: RegistrationStage::InitialRegister,
            source: SipError::MissingTransactionHeader(marker),
        },
    ];
    for error in errors {
        let diagnostic = registration(&error);
        let wire = serde_json::to_string(&diagnostic).unwrap();
        assert!(!wire.contains("SECRET"));
        assert!(!format!("{diagnostic:?}").contains("SECRET"));
    }
}

#[test]
fn ims_io_failure_reports_only_static_operation_kind_and_numeric_os_error() {
    let diagnostic = registration(&RegistrationError::<()>::Io {
        stage: RegistrationStage::PlainConnect,
        source: std::io::Error::from(std::io::ErrorKind::TimedOut),
    });
    assert_eq!(diagnostic.stage, "ims_transport");
    assert_eq!(diagnostic.io_operation, Some("plain_connect"));
    assert_eq!(diagnostic.io_kind, Some("timed_out"));
}

#[test]
fn registration_failure_adds_only_bounded_packet_counts() {
    let failure = NativeRegisteredStartupFailure::Registration {
        error: NativeImsRegistrationError::TransportClosed,
        network: Ok(Ok(())),
        network_ended_before_cleanup: false,
        local_retired: true,
        traffic: proletarius_vowifi::TunnelTrafficSnapshot {
            sent_esp: 8,
            received_esp: 2,
            accepted_ip: 1,
            rejected_esp: 1,
            received_ike: 3,
            received_keepalive: 4,
            invalid_datagram: 5,
        },
    };
    let diagnostic = startup(&failure);
    let wire = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(wire["esp_sent"], 8);
    assert_eq!(wire["esp_received"], 2);
    assert_eq!(wire["esp_rejected"], 1);
    assert_eq!(wire["ip_accepted"], 1);
    assert_eq!(wire["network_ended_before_cleanup"], false);
    assert_eq!(wire["ike_received_after_child"], 3);
    assert_eq!(wire["keepalive_received_after_child"], 4);
    assert_eq!(wire["invalid_datagram_after_child"], 5);
    assert_eq!(wire["code"], "transport_closed");
    assert!(wire.get("address").is_none());
    assert!(wire.get("key").is_none());
    assert!(
        serde_json::to_value(startup(&NativeRegisteredStartupFailure::SessionMismatch))
            .unwrap()
            .get("esp_sent")
            .is_none()
    );
}

#[test]
fn pre_cleanup_packet_transform_exit_is_bounded_and_keeps_io_kind() {
    let failure = NativeRegisteredStartupFailure::Registration {
        error: NativeImsRegistrationError::TransportClosed,
        network: Ok(Err(NativeNetworkError::Network(
            proletarius_vowifi::netstack_runtime::NetworkRunError::PacketTransform(
                std::io::Error::from(std::io::ErrorKind::InvalidData),
            ),
        ))),
        network_ended_before_cleanup: true,
        local_retired: true,
        traffic: proletarius_vowifi::TunnelTrafficSnapshot {
            sent_esp: 1,
            received_esp: 1,
            accepted_ip: 1,
            rejected_esp: 0,
            received_ike: 0,
            received_keepalive: 0,
            invalid_datagram: 0,
        },
    };
    let wire = serde_json::to_value(startup(&failure)).unwrap();
    assert_eq!(wire["network_ended_before_cleanup"], true);
    assert_eq!(wire["network_exit"], "packet_transform");
    assert_eq!(wire["network_io_kind"], "invalid_data");
    assert!(wire.get("network_error").is_none());
    assert!(wire.get("packet").is_none());
}

#[test]
fn pre_cleanup_ike_exit_is_bounded_and_keeps_the_typed_data_plane_reason() {
    let failure = NativeRegisteredStartupFailure::Registration {
        error: NativeImsRegistrationError::TransportClosed,
        network: Ok(Err(NativeNetworkError::Tunnel(IkeAuthError::DataPlane(
            proletarius_vowifi::ChildDataError::EventConsumerUnavailable,
        )))),
        network_ended_before_cleanup: true,
        local_retired: true,
        traffic: Default::default(),
    };
    let wire = serde_json::to_value(startup(&failure)).unwrap();
    assert_eq!(wire["network_ended_before_cleanup"], true);
    assert_eq!(wire["network_exit"], "ike_tunnel");
    assert_eq!(wire["ike_tunnel_exit"], "event_consumer_unavailable");
    assert!(wire.get("network_error").is_none());
    assert!(wire.get("packet").is_none());
}

#[test]
fn registration_deadline_and_cancellation_remain_distinct() {
    let deadline: RegistrationError<()> =
        RegistrationError::Deadline(RegistrationStage::ProtectedListen);
    let cancelled: RegistrationError<()> =
        RegistrationError::Cancelled(RegistrationStage::Authenticate);
    assert_eq!(
        (registration(&deadline).stage, registration(&deadline).code),
        ("ims_incoming_listener", "timeout")
    );
    assert_eq!(
        (
            registration(&cancelled).stage,
            registration(&cancelled).code
        ),
        ("ims_aka", "cancelled")
    );
}
