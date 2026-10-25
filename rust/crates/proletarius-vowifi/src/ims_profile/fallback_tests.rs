use super::*;
use crate::carrier_overrides::OverrideSet;
fn fixture() -> (NativeImsRegisterProfile, NativeImsDeviceIdentity) {
    let sim = SimIdentity {
        mcc: "234".into(),
        mnc: "33".into(),
        imsi: "234330000000001".into(),
        ..Default::default()
    };
    let carrier = crate::carrier::Registry::embedded()
        .unwrap()
        .resolve(&sim)
        .unwrap()
        .unwrap();
    let p = carrier.native_ims_register_profile(&sim, None).unwrap();
    let d = p.resolve_device_identity(Some("353936932685355")).unwrap();
    (p, d)
}
fn runtime() -> NativeImsRuntime<'static> {
    NativeImsRuntime {
        imsi: "234330000000001",
        mcc: "234",
        mnc: "33",
        local_ip: "10.0.0.2".parse().unwrap(),
        pcscf: "10.0.0.9:5060".parse().unwrap(),
        call_id: "fallback-fixture",
        from_tag: "r",
        via_branch: "z9hG4bK-f",
        cell_id: None,
        isim_private_identity: None,
        isim_public_identity: None,
    }
}
fn endpoints() -> NativeSecurityEndpoints {
    NativeSecurityEndpoints {
        spi_c: 1,
        spi_s: 2,
        port_c: 5064,
        port_s: 5063,
    }
}
#[test]
fn opt_in_preserves_existing_policy_defaults_and_explicit_empty_statuses() {
    let (p, _) = fixture();
    assert_eq!(
        p.initial_register_fallback(false, Some(403)).variants.len(),
        1
    );
    let enabled = p.initial_register_fallback(true, Some(403));
    assert!(enabled.retry);
    assert_eq!(enabled.variants.len(), 5);
    let sim = SimIdentity {
        mcc: "234".into(),
        mnc: "33".into(),
        imsi: "234330000000001".into(),
        ..Default::default()
    };
    let carrier = crate::carrier::Registry::embedded()
        .unwrap()
        .resolve(&sim)
        .unwrap()
        .unwrap();
    let overrides=OverrideSet::parse(b"carrier_overrides:\n  '234/33': {ims_register_template: {enable_initial_reject_fallback: true, register_policy: {initial_reject_fallback_status_codes: []}}}\n").unwrap();
    let p = carrier
        .native_ims_register_profile(&sim, overrides.get("234", "33"))
        .unwrap();
    assert_eq!(
        p.initial_register_fallback(false, Some(403)).variants.len(),
        5
    );
    assert!(!p.initial_register_fallback(false, Some(403)).retry);
}
#[test]
fn variants_compile_exact_initial_headers_without_changing_profile_security() {
    let (p, d) = fixture();
    let r = runtime();
    let sub = p.resolve_subscriber_identity(r).unwrap();
    let variants = p.initial_register_fallback(true, None).variants;
    for (i, v) in variants.iter().enumerate() {
        let wire = p
            .compile_with_initial_variant(
                &d,
                r,
                &sub,
                NativeImsRegisterAttempt::Initial {
                    cseq: i as u32 + 1,
                    local_port: 5060,
                    endpoints: endpoints(),
                },
                None,
                Some(v),
            )
            .unwrap();
        let text = String::from_utf8(wire.to_vec()).unwrap();
        assert_eq!(text.contains("P-Access-Network-Info:"), v.include_pani);
        assert_eq!(text.contains("Cellular-Network-Info:"), v.include_cellular);
        assert!(text.contains("Require: sec-agree\r\nProxy-Require: sec-agree\r\n"));
        assert!(text.contains("Security-Client:"));
        match i {
            3 => assert!(text.contains("response=\"00000000000000000000000000000000\"")),
            4 => assert!(!text.contains("Authorization:")),
            _ => assert!(text.contains("response=\"\"")),
        }
    }
}
#[test]
fn rejected_variant_cannot_change_authenticated_or_protected_request() {
    let (p, d) = fixture();
    let r = runtime();
    let sub = p.resolve_subscriber_identity(r).unwrap();
    let res=proletarius_ims::SipResponse::parse(b"SIP/2.0 401 Unauthorized\r\nCall-ID: fallback-fixture\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims.mnc033.mcc234.3gppnetwork.org\",nonce=\"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\",algorithm=AKAv1-MD5\r\nContent-Length: 0\r\n\r\n").unwrap();
    let auth = proletarius_ims::aka_authorization::authorize_response(
        &res,
        &proletarius_ims::aka_authorization::AuthorizationRequest {
            username: sub.private_identity(),
            method: "REGISTER",
            uri: "sip:ims.mnc033.mcc234.3gppnetwork.org",
            body: b"",
            cnonce: "fixture",
            nonce_count: 1,
        },
        proletarius_ims::aka_digest::AkaPassword::Res(&[7; 8]),
    )
    .unwrap();
    let none = p
        .initial_register_fallback(true, None)
        .variants
        .pop()
        .unwrap();
    for protected in [false, true] {
        let make = || {
            if protected {
                NativeImsRegisterAttempt::Protected {
                    cseq: 3,
                    endpoints: endpoints(),
                    authorization: &auth,
                    security_verify: "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc",
                    remote_port_s: 6060,
                }
            } else {
                NativeImsRegisterAttempt::PlainAuthenticated {
                    cseq: 2,
                    local_port: 5060,
                    endpoints: endpoints(),
                    authorization: &auth,
                    security_disposition: PlainSecurityDisposition::RetainOffer,
                }
            }
        };
        let original = p
            .compile_with_initial_variant(&d, r, &sub, make(), None, None)
            .unwrap();
        let overridden = p
            .compile_with_initial_variant(&d, r, &sub, make(), None, Some(&none))
            .unwrap();
        assert_eq!(original, overridden);
    }
}
#[test]
fn invalid_variant_and_missing_eap_material_fail_before_any_wire() {
    let (p, d) = fixture();
    let r = runtime();
    let sub = p.resolve_subscriber_identity(r).unwrap();
    for auth in ["unknown", "eap_direct"] {
        let v = proletarius_ims::register_fallback::InitialRegisterVariant {
            initial_auth: auth.into(),
            include_pani: false,
            include_cellular: false,
        };
        assert!(
            p.compile_with_initial_variant(
                &d,
                r,
                &sub,
                NativeImsRegisterAttempt::Initial {
                    cseq: 1,
                    local_port: 5060,
                    endpoints: endpoints()
                },
                None,
                Some(&v)
            )
            .is_err()
        );
    }
}
