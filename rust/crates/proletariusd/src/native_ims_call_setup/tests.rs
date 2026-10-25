use super::*;
use crate::native_ims_actor::Command;
use crate::native_ims_calls::{CallBinding, LocalMedia, Media, MediaFuture, Service};
use crate::native_vowifi_supervisor::SessionGeneration;
use proletarius_ims::stream::{SipStreamItem, SipStreamReader};
use proletarius_sip::{Message, Parser};
use proletarius_vowifi::{
    TunnelConfiguration,
    carrier::{Registry, SimIdentity},
    carrier_overrides::OverrideSet,
    ims_profile::{NativeImsRuntime, NativeSecurityEndpoints},
    netstack_runtime::TunnelNetwork,
    netstack_tcp::TcpSocketRegistry,
    registration_coordinator::{
        RegistrationServices, register_native_profile_with_identity_and_eap_direct_material,
    },
    security_runtime::SecurityNetworkRuntime,
    sip_session_runtime::RegisteredSipRuntime,
};
use smoltcp::iface::SocketSet;
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

struct OfferMedia;
impl Media for OfferMedia {
    fn allocate(self: Arc<Self>, _: CallBinding) -> MediaFuture<LocalMedia> {
        Box::pin(async {
            Ok(LocalMedia {
                ip: "10.0.0.2".parse().unwrap(),
                port: 49170,
                audio_port: 0,
                audio_token: String::new(),
            })
        })
    }
    fn configure(
        self: Arc<Self>,
        _: CallBinding,
        _: proletarius_call::sdp::AudioNegotiation,
    ) -> MediaFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn start(self: Arc<Self>, _: CallBinding, _: bool) -> MediaFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn stop(self: Arc<Self>, _: CallBinding) -> MediaFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn dtmf(self: Arc<Self>, _: CallBinding, _: char) -> MediaFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

async fn actual_register_and_dialog(mcc: &str, mnc: &str, number: bool, selector: bool) {
    let configuration = |address: &str| TunnelConfiguration {
        ipv4_addresses: vec![address.parse().unwrap()],
        ..Default::default()
    };
    let local = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
    let remote = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
    let (lw_tx, lw) = watch::channel(0_u64);
    let (rw_tx, rw) = watch::channel(0_u64);
    let (ltcp, mut lr) = TcpSocketRegistry::new(lw_tx.clone());
    let (rtcp, rr) = TcpSocketRegistry::new(rw_tx);
    let (security, runtime) = SecurityNetworkRuntime::new(
        move |interface: &mut smoltcp::iface::Interface, sockets: &mut SocketSet<'static>| {
            lr.service(interface, sockets);
            Ok::<_, Infallible>(())
        },
        lw_tx,
    );
    let (lo, ri) = mpsc::channel(64);
    let (ro, li) = mpsc::channel(64);
    let (stop_tx, stop) = watch::channel(false);
    let local_runner = local.run_managed(SocketSet::new(vec![]), li, lo, stop.clone(), lw, runtime);
    let remote_runner = remote.run_managed(SocketSet::new(vec![]), ri, ro, stop, rw, rr);
    let (ready_tx, ready) = oneshot::channel();
    let (wire_tx, wire_rx) = oneshot::channel();
    let server = async {
        let listener = rtcp
            .listen(5060, Some("10.0.0.2:5050".parse().unwrap()))
            .await
            .unwrap();
        ready_tx.send(()).unwrap();
        let stream = listener.accept().await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = SipStreamReader::new(read);
        let Some(SipStreamItem::Message(register)) = reader.next().await.unwrap() else {
            panic!("missing actual REGISTER")
        };
        let (Message::Request(register), consumed) =
            Parser::default().parse(&register, false).unwrap()
        else {
            panic!("not REGISTER")
        };
        assert_eq!(consumed, Message::Request(register.clone()).encode().len());
        assert_eq!(register.method.as_str(), "REGISTER");
        let contact = register
            .data
            .headers
            .get("Contact")
            .unwrap()
            .value()
            .to_owned();
        assert!(contact.contains(":5050"));
        let associated = if number {
            "P-Associated-URI: <sip:alternate@ims.example>, <tel:+447700900123>\r\n"
        } else {
            "P-Associated-URI: <sip:alternate@ims.example>\r\n"
        };
        let response = format!(
            "SIP/2.0 200 OK\r\nCall-ID: setup-real-register\r\nCSeq: 1 REGISTER\r\nContact: {contact}\r\nExpires: 7200\r\n{associated}Service-Route: <sip:first@ims.example;lr>\r\nService-Route: <sip:second@ims.example;lr>\r\nContent-Length: 0\r\n\r\n"
        );
        write.write_all(response.as_bytes()).await.unwrap();
        write.flush().await.unwrap();
        let mut seen = vec![];
        for _ in 0..2 {
            let Some(SipStreamItem::Message(wire)) = reader.next().await.unwrap() else {
                panic!("missing dialog")
            };
            let (Message::Request(request), consumed) =
                Parser::default().parse(&wire, false).unwrap()
            else {
                panic!("not request")
            };
            assert_eq!(consumed, wire.len());
            assert_eq!(
                request.data.headers.get("Contact").unwrap().value(),
                contact
            );
            seen.push(request);
        }
        wire_tx
            .send(seen)
            .unwrap_or_else(|_| panic!("wire observer abandoned"));
        assert!(reader.next().await.unwrap().is_none());
        reader.into_inner().unsplit(write).close().await.unwrap();
    };
    let client = async {
        ready.await.unwrap();
        let sim = SimIdentity {
            mcc: mcc.into(),
            mnc: mnc.into(),
            imsi: format!("{mcc}{mnc}0000000001"),
            ..Default::default()
        };
        let registry = Registry::embedded().unwrap();
        let carrier = registry.resolve(&sim).unwrap().unwrap();
        let runtime_carrier =
            RuntimeCarrierProfile::expand(&carrier, &sim, &registry.revision).unwrap();
        let template_id = if selector { ", id: 3_hk" } else { "" };
        let override_yaml = format!(
            "carrier_overrides:\n  '{mcc}/{mnc}': {{ims_domain: ims.example, ims_realm: ims.example, ims_local_port: 5050, ims_identity_source: isim, ims_register_template: {{sec_agree_mode: disabled{template_id}}}}}\n"
        );
        let overrides = OverrideSet::parse(override_yaml.as_bytes()).unwrap();
        let profile = carrier
            .native_ims_register_profile(&sim, overrides.get(mcc, mnc))
            .unwrap();
        let device = profile
            .resolve_device_identity(Some("353936932685355"))
            .unwrap();
        let input = NativeImsRuntime {
            imsi: &sim.imsi,
            isim_private_identity: Some("subscriber@ims.example"),
            isim_public_identity: Some("sip:configured@ims.example"),
            local_ip: "10.0.0.2".parse().unwrap(),
            pcscf: "10.20.30.9:5060".parse().unwrap(),
            call_id: "setup-real-register",
            from_tag: "setup-tag",
            via_branch: "z9hG4bK-setup",
            cell_id: None,
            mcc,
            mnc,
        };
        let subscriber = profile.resolve_subscriber_identity(input).unwrap();
        let (_session_stop_tx, session_stop) = watch::channel(false);
        let (events, _output) = mpsc::channel(8);
        let session = register_native_profile_with_identity_and_eap_direct_material(
            RegistrationServices::new(&ltcp, &security, &events),
            &profile,
            &device,
            &subscriber,
            input,
            NativeSecurityEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5064,
                port_s: 5063,
            },
            None,
            Instant::now() + Duration::from_secs(4),
            session_stop,
            |_, _| async {
                Err::<proletarius_ims::aka_exchange::SimAnswer, _>("unexpected challenge")
            },
        )
        .await
        .unwrap_or_else(|_| panic!("actual REGISTER failed"));
        assert!(!session.is_secured());
        let context = session.call_context().unwrap();
        assert_eq!(context.port_c(), 5050);
        assert_eq!(context.port_s(), 5050);
        assert_eq!(context.register_cseq(), 1);
        assert_eq!(context.public_identity(), "sip:configured@ims.example");
        let config = prepare(&session, &profile, &runtime_carrier, false).unwrap();
        assert_eq!(config.ussd_service_selector, selector);
        let mut foreign = runtime_carrier.clone();
        foreign.id = "foreign".into();
        assert_eq!(
            prepare(&session, &profile, &foreign, false).err().unwrap(),
            "registered_call_profile_mismatch"
        );
        let sms_config = prepare(&session, &profile, &runtime_carrier, true).unwrap();
        assert!(sms_config.sms_only);
        let generation = SessionGeneration::from_value(9).unwrap();
        let (commands, mut input) = mpsc::channel(8);
        let sender = async {
            let mut runtime = RegisteredSipRuntime::new(session);
            for _ in 0..2 {
                let Some(Command::SendUntracked {
                    generation: captured,
                    request,
                    reply,
                }) = input.recv().await
                else {
                    panic!("unexpected command")
                };
                assert_eq!(captured, generation);
                let result = runtime.send_dialog_request(request).await;
                reply
                    .send(result)
                    .unwrap_or_else(|_| panic!("send receipt abandoned"));
            }
            let wire = wire_rx.await.unwrap();
            runtime.shutdown().await.unwrap();
            wire
        };
        let caller = async {
            let mut calls = Service::new(generation, config, commands, Arc::new(OfferMedia))
                .unwrap_or_else(|_| panic!("config rejected"));
            calls.dial(generation, "+447700900456").await.unwrap();
            calls.start_ussd(generation, "*123#").await.unwrap();
            assert!(
                calls.active(),
                "USSD cannot be interrupted by REGISTER renewal"
            );
        };
        let (wire, ()) = tokio::join!(sender, caller);
        let voice = &wire[0];
        let ussd = &wire[1];
        assert_eq!(
            voice.data.headers.get("Route").unwrap().value(),
            "<sip:10.20.30.9:5060;lr>"
        );
        let expected_from = if number {
            "<sip:+447700900123@ims.example;user=phone>;tag="
        } else {
            "<sip:configured@ims.example>;tag="
        };
        assert!(
            voice
                .data
                .headers
                .get("From")
                .unwrap()
                .value()
                .starts_with(expected_from)
        );
        assert_eq!(
            voice
                .data
                .headers
                .get("P-Preferred-Identity")
                .unwrap()
                .value(),
            if number {
                "<tel:+447700900123>"
            } else {
                "<sip:configured@ims.example>"
            }
        );
        assert_eq!(
            ussd.data
                .headers
                .all("Route")
                .map(|h| h.value())
                .collect::<Vec<_>>(),
            vec!["<sip:first@ims.example;lr>", "<sip:second@ims.example;lr>"]
        );
        assert!(!ussd.data.headers.get("P-Access-Network-Info").is_some());
        assert_eq!(
            ussd.data.headers.get("Accept-Contact").unwrap().value(),
            if selector {
                "*;+g.3gpp.ussd"
            } else {
                "*;+g.3gpp.icsi-ref=\"urn%3Aurn-7%3A3gpp-service.ims.icsi.mmtel\""
            }
        );
        assert_eq!(
            ussd.data.headers.get("P-Preferred-Service").is_some(),
            selector
        );
        assert!(
            ussd.uri
                .to_string()
                .contains("*123%23;phone-context=ims.example@ims.example;user=dialstring")
        );
        stop_tx.send(true).unwrap();
    };
    tokio::time::timeout(Duration::from_secs(8), async {
        let (local, remote, (), ()) = tokio::join!(local_runner, remote_runner, server, client);
        local.unwrap();
        remote.unwrap();
    })
    .await
    .expect("actual REGISTER/dialog timed out");
}

#[tokio::test(flavor = "current_thread")]
async fn successful_register_authorizes_msisdn_contact_and_ordered_ussd_routes() {
    actual_register_and_dialog("234", "33", true, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn no_e164_associated_uri_keeps_configured_impu() {
    actual_register_and_dialog("234", "33", false, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn actual_register_with_three_hk_selector_override_preserves_ussi() {
    actual_register_and_dialog("234", "33", true, true).await;
}
