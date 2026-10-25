use super::*;
use crate::reader_reservation::SelectionSnapshot;

fn root() -> PathBuf {
    let root = PathBuf::from(env::var_os("SWITCH_DATA_DIR").expect("isolated workspace runtime"))
        .join(format!(
            "configuration-preview-{}",
            crate::snapshot::new_instance_id()
        ));
    fs::create_dir_all(&root).unwrap();
    root
}
fn snapshot(metadata: Option<Value>) -> crate::reader_lifecycle::Snapshot {
    crate::reader_lifecycle::Snapshot {
        status: json!({}),
        usim_info: metadata,
        selection: SelectionSnapshot::default(),
        esim_manager_available: false,
        notification_aids: vec![],
        esim_profile_name: String::new(),
        pcsc_session: None,
    }
}
fn metadata() -> Value {
    json!({"iccid":"8944330000000000001","imsi":"234330123456789",
    "mcc":"234","mnc":"33","operator_name":"example","operator_source":"network"})
}
fn prepared(identity: &LeaseIdentity, yaml: &[u8]) -> Result<Resolved, PlanError> {
    resolve(
        identity,
        "",
        None,
        OverrideSet::parse(yaml).unwrap(),
        || Ok(Some(Zeroizing::new("356938035643809".into()))),
    )
}

#[test]
fn module_and_reader_share_one_carrier_resolution_path_for_supported_cards() {
    struct Case {
        mcc: &'static str,
        mnc: &'static str,
        imsi: &'static str,
        gid1: &'static str,
        spn: &'static str,
        profile_id: &'static str,
        epdg: &'static str,
    }

    for case in [
        Case {
            mcc: "234",
            mnc: "33",
            imsi: "234330123456789",
            gid1: "",
            spn: "CTExcel",
            profile_id: "apple_orange_uk",
            epdg: "epdg.epc.mnc033.mcc234.pub.3gppnetwork.org",
        },
        Case {
            mcc: "234",
            mnc: "15",
            imsi: "234150123456789",
            gid1: "",
            spn: "Vodafone",
            profile_id: "apple_vodafone_uk",
            epdg: "epdg.epc.mnc015.mcc234.pub.3gppnetwork.org",
        },
        Case {
            mcc: "234",
            mnc: "20",
            imsi: "234200123456789",
            gid1: "",
            spn: "3 UK",
            profile_id: "apple_hutchison_uk",
            epdg: "epdg.epc.mnc020.mcc234.pub.3gppnetwork.org",
        },
        Case {
            mcc: "234",
            mnc: "10",
            imsi: "234100123456789",
            gid1: "508FFFFF00",
            spn: "giffgaff",
            profile_id: "apple_o2_giffgaff_uk",
            epdg: "epdg.epc.mnc010.mcc234.pub.3gppnetwork.org",
        },
        Case {
            mcc: "234",
            mnc: "30",
            imsi: "234300123456789",
            gid1: "",
            spn: "CMLink",
            profile_id: "apple_tmobile_uk",
            epdg: "epdg.epc.mnc030.mcc234.pub.3gppnetwork.org",
        },
        Case {
            mcc: "234",
            mnc: "10",
            imsi: "234100123456789",
            gid1: "",
            spn: "Loop",
            profile_id: "apple_o2_uk",
            epdg: "epdg.epc.mnc010.mcc234.pub.3gppnetwork.org",
        },
        Case {
            mcc: "454",
            mnc: "03",
            imsi: "454030123456789",
            gid1: "",
            spn: "SIM World",
            profile_id: "apple_hutchison_hk",
            epdg: "wlan.three.com.hk",
        },
    ] {
        let domain = format!("ims.mnc{:0>3}.mcc{}.3gppnetwork.org", case.mnc, case.mcc);
        let identity = snapshot_identity(&json!({
            "iccid": format!("89{}{}000000000001", case.mcc, case.mnc),
            "imsi": case.imsi,
            "mcc": case.mcc,
            "mnc": case.mnc,
            "gid1": case.gid1,
            "operator_name": case.spn,
            "operator_source": "ef_spn",
            "private_identity": format!("{}@{}", case.imsi, domain),
            "public_identity": format!("sip:{}@{}", case.imsi, domain),
            "home_domain": domain,
        }));

        // Module and reader preparation both enter this transport-free resolver;
        // only the identity acquisition adapter differs before this boundary.
        let resolved = prepared(&identity, b"carrier_overrides: {}\n").unwrap();
        assert_eq!(resolved.carrier_id, case.profile_id, "{}", case.spn);
        assert_eq!(resolved.config.epdg, case.epdg, "{}", case.spn);
        assert_eq!(
            resolved.status["profile_id"], case.profile_id,
            "{}",
            case.spn
        );
    }
}

#[test]
fn idle_without_metadata_does_not_create_device_identity_or_any_owner() {
    let root = root();
    let value = preview_reader_status(&snapshot(None), &root).ok().unwrap();
    assert_eq!(value, json!({"available":false}));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn snapshot_identity_does_not_promote_network_operator_name_to_spn() {
    let identity = snapshot_identity(&metadata());
    assert!(identity.spn.is_empty());
    assert_eq!(identity.imsi.as_str(), "234330123456789");
}
#[test]
fn preview_and_live_resolver_share_override_identity_and_carrier_policy() {
    let identity = snapshot_identity(&metadata());
    let yaml=b"carrier_overrides:\n  '234/33': {custom_epdg: 'proxy-preserved.invalid', epdg_port: 500, apn: 'custom-ims', ip_stack: ipv4, algorithm_policy: strict, device_model: 'iphone15,4', device_identity_imei: '353936932685355'}\n";
    let frozen = prepared(&identity, yaml).ok().unwrap();
    assert_eq!(frozen.status["epdg_fqdn"], "proxy-preserved.invalid");
    assert_eq!(frozen.status["imei"], "353936932685355");
    assert_eq!(
        frozen
            .ike_device_identity_imei
            .as_deref()
            .map(String::as_str),
        Some("353936932685355")
    );
    assert_eq!(frozen.config.epdg_port, 500);
    assert_eq!(frozen.status["algorithm_policy"], "strict");
    assert_eq!(frozen.status["device_model"], "iphone15,4");
    assert_eq!(frozen.status["ims_identity_source"], "derived");
    assert_eq!(
        frozen.status["ims_transport"],
        frozen.ims.transport().as_str()
    );
    assert!(!frozen.subscriber_identity.uses_isim());
    // Later YAML changes cannot mutate the already-prepared session projection.
    let later = prepared(&identity, b"carrier_overrides: {}\n")
        .ok()
        .unwrap();
    assert_ne!(frozen.status["epdg_fqdn"], later.status["epdg_fqdn"]);
    assert_eq!(frozen.status["epdg_fqdn"], frozen.config.epdg);
}

#[test]
fn ike_device_identity_obeys_carrier_disable_without_new_failure() {
    let identity = snapshot_identity(&json!({
        "iccid":"8944200000000000001", "imsi":"234200123456789", "mcc":"234", "mnc":"20"
    }));
    for persisted in [None, Some("356938035643809")] {
        let overrides = OverrideSet::parse(
            b"carrier_overrides:\n  '234/20': {device_identity_enabled: false}\n",
        )
        .unwrap();
        let resolved = resolve(&identity, "", None, overrides, || {
            Ok(persisted.map(|value| Zeroizing::new(value.to_owned())))
        })
        .unwrap();
        assert!(resolved.ike_device_identity_imei.is_none());
        assert!(!resolved.device_identity.enabled());
    }
}

#[test]
fn ike_device_identity_keeps_existing_missing_or_invalid_session_errors() {
    let identity = snapshot_identity(&metadata());
    for persisted in [None, Some("not-an-imei")] {
        let result = resolve(
            &identity,
            "",
            None,
            OverrideSet::parse(b"carrier_overrides: {}\n").unwrap(),
            || Ok(persisted.map(|value| Zeroizing::new(value.to_owned()))),
        );
        assert!(matches!(result, Err(PlanError::DeviceIdentity(_))));
    }
}
#[test]
fn partial_isim_and_unsupported_e911_fail_before_device_side_effects() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut identity = snapshot_identity(&metadata());
    identity.private_identity = Zeroizing::new("secret@ims.invalid".into());
    let called = AtomicBool::new(false);
    let result = resolve(
        &identity,
        "",
        None,
        OverrideSet::parse(b"carrier_overrides:\n  '234/33': {ims_identity_source: auto}\n")
            .unwrap(),
        || {
            called.store(true, Ordering::SeqCst);
            Ok(None)
        },
    );
    assert!(matches!(result, Err(PlanError::SubscriberIdentity(_))));
    assert!(!called.load(Ordering::SeqCst));
    let result = prepared(
        &identity,
        b"carrier_overrides:\n  '234/33': {e911: {enabled: true, provider: att_entitlement, entitlement_url: 'https://example.net/', websheet_host_policy: public_https}}\n",
    );
    assert!(matches!(result, Err(PlanError::Carrier)));
}
#[test]
fn idle_preview_persists_same_imei_and_reports_configuration_errors_without_secrets() {
    assert!(
        env::var_os(OVERRIDES_ENVIRONMENT).is_none(),
        "fixture requires default config path"
    );
    let root = root();
    let value = preview_reader_status(&snapshot(Some(metadata())), &root)
        .ok()
        .unwrap();
    assert_eq!(value["available"], true);
    assert_eq!(value["mcc"], "234");
    assert_eq!(
        value["imei"],
        crate::reader_device_identity::Store::new(&root)
            .resolve_or_create()
            .unwrap()
    );
    fs::write(root.join("carrier-overrides.yaml"), b"not-valid: [").unwrap();
    let error = preview_reader_status(&snapshot(Some(metadata())), &root)
        .err()
        .unwrap();
    assert_eq!(error.code(), "native_carrier_overrides_invalid");
    let projected = error.status_projection();
    assert_eq!(projected["available"], true);
    assert_eq!(projected["imei"], value["imei"]);
    assert!(!projected.to_string().contains("not-valid"));
    fs::write(
        root.join("carrier-overrides.yaml"),
        b"carrier_overrides:\n  '234/33': {e911: {enabled: true, provider: att_entitlement, entitlement_url: 'https://example.net/', websheet_host_policy: public_https}}\n",
    )
    .unwrap();
    let error = preview_reader_status(&snapshot(Some(metadata())), &root)
        .err()
        .unwrap();
    assert_eq!(error.status_projection()["e911_status"], "unsupported");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn complete_isim_pair_is_only_reported_when_effective_policy_uses_it() {
    let mut identity = snapshot_identity(&metadata());
    identity.private_identity = Zeroizing::new("impi@ims.invalid".into());
    identity.public_identity = Zeroizing::new("sip:impu@ims.invalid".into());
    let derived = prepared(
        &identity,
        b"carrier_overrides:\n  '234/33': {ims_identity_source: derived}\n",
    )
    .ok()
    .unwrap();
    assert_eq!(derived.status["ims_identity_source"], "derived");
    let isim = prepared(
        &identity,
        b"carrier_overrides:\n  '234/33': {ims_identity_source: auto}\n",
    )
    .ok()
    .unwrap();
    assert!(isim.subscriber_identity.uses_isim());
    assert_eq!(isim.status["ims_identity_source"], "isim");
    assert!(!isim.status.to_string().contains("impi@"));
}

#[test]
fn explicit_absolute_override_does_not_resolve_default_data_root_or_cwd() {
    let explicit = PathBuf::from("/workspace/build/runtime/carrier-fixture.yaml");
    let result = resolve_path(
        Some(explicit.as_os_str()),
        Some(std::ffi::OsStr::new("ignored.yaml")),
        || panic!("absolute source must not need cwd"),
        || panic!("explicit source must not need default root"),
    );
    assert_eq!(result.unwrap(), explicit);
    // Archived Go newVoWiFiOverrideStore uses filepath.Abs for relative CLI
    // AND environment paths. The former absolute-only rejection was a Rust
    // compatibility regression, not a source contract.
    for (cli, environment) in [(Some("relative.yaml"), None), (None, Some("relative.yaml"))] {
        assert_eq!(
            resolve_path(
                cli.map(std::ffi::OsStr::new),
                environment.map(std::ffi::OsStr::new),
                || Ok(PathBuf::from("/startup")),
                || panic!("explicit relative source must not need default root")
            )
            .unwrap(),
            PathBuf::from("/startup/relative.yaml")
        );
    }
}

#[test]
fn trimmed_cli_then_environment_then_default_paths_are_lexically_clean() {
    let cases = [
        (
            Some("  ./cli/../chosen.yaml "),
            Some("env.yaml"),
            "/startup/chosen.yaml",
        ),
        (Some("\u{3000}"), Some("  ../env.yaml\n"), "/env.yaml"),
        (
            Some(""),
            Some(" /absolute/a/../../chosen.yaml "),
            "/chosen.yaml",
        ),
        (None, Some("\t"), "/workspace-data/carrier-overrides.yaml"),
        (Some("  "), None, "/workspace-data/carrier-overrides.yaml"),
        (None, None, "/workspace-data/carrier-overrides.yaml"),
        (
            Some("/../../missing/./chosen.yaml"),
            None,
            "/missing/chosen.yaml",
        ),
        (Some("link/../chosen.yaml"), None, "/startup/chosen.yaml"),
    ];
    for (cli, environment, expected) in cases {
        assert_eq!(
            resolve_path(
                cli.map(std::ffi::OsStr::new),
                environment.map(std::ffi::OsStr::new),
                || Ok(PathBuf::from("/startup")),
                || Ok(PathBuf::from("/workspace-data"))
            )
            .unwrap(),
            PathBuf::from(expected)
        );
    }
    assert!(matches!(
        resolve_path(
            None,
            None,
            || panic!("default failure stays primary"),
            || Err(PlanError::Overrides)
        ),
        Err(PlanError::Overrides)
    ));
    assert!(matches!(
        resolve_path(
            Some(std::ffi::OsStr::new("relative.yaml")),
            None,
            || Err(PlanError::Overrides),
            || panic!("cwd failure must not fall back")
        ),
        Err(PlanError::Overrides)
    ));
}

#[test]
fn cli_selected_file_drives_epdg_and_is_reloaded_without_stale_or_environment_fallback() {
    let root = root();
    let env_path = root.join("env.yaml");
    let cli_path = root.join("chosen.yaml");
    fs::write(
        &env_path,
        b"carrier_overrides:\n  '234/33': {custom_epdg: 'environment.invalid'}\n",
    )
    .unwrap();
    let arguments = crate::daemon_args::parse([
        "--vowifi-config".into(),
        " ./ignored/../chosen.yaml ".into(),
    ])
    .unwrap();
    let path = resolve_path(
        arguments.vowifi_config.as_deref(),
        Some(env_path.as_os_str()),
        || Ok(root.clone()),
        || panic!("selected CLI must not read default"),
    )
    .unwrap();
    assert_eq!(path, cli_path);
    let identity = snapshot_identity(&metadata());
    let load = || {
        resolve(&identity, "", None, read_overrides(&path)?, || {
            Ok(Some(Zeroizing::new("356938035643809".into())))
        })
    };
    // Missing selected file is an empty set, not selection of the env file.
    assert!(read_overrides(&path).unwrap().get("234", "33").is_none());
    fs::write(
        &cli_path,
        b"carrier_overrides:\n  '234/33': {custom_epdg: 'cli-first.invalid'}\n",
    )
    .unwrap();
    let frozen = load().ok().unwrap();
    assert_eq!(frozen.status["epdg_fqdn"], "cli-first.invalid");
    fs::write(
        &cli_path,
        b"carrier_overrides:\n  '234/33': {custom_epdg: 'cli-second.invalid'}\n",
    )
    .unwrap();
    assert_eq!(
        load().ok().unwrap().status["epdg_fqdn"],
        "cli-second.invalid"
    );
    assert_eq!(frozen.status["epdg_fqdn"], "cli-first.invalid");
    fs::write(&cli_path, b"carrier_overrides: [malformed").unwrap();
    assert!(matches!(load(), Err(PlanError::Overrides)));
    // The valid env file must neither mask malformed selected YAML nor supply
    // overrides when the selected file is subsequently empty or removed.
    fs::write(&cli_path, b" \n\t").unwrap();
    assert!(read_overrides(&path).unwrap().get("234", "33").is_none());
    fs::remove_file(&cli_path).unwrap();
    assert!(read_overrides(&path).unwrap().get("234", "33").is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unicode_filename_reads_yaml_and_invalid_byte_path_serializes_failure() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = root();
    let valid = root.join("carrier-有效.yaml");
    let selected = resolve_path(
        crate::daemon_args::parse([
            OsString::from("-vowifi-config"),
            valid.clone().into_os_string(),
        ])
        .unwrap()
        .vowifi_config
        .as_deref(),
        None,
        || panic!("absolute path needs no cwd"),
        || panic!("selected path needs no default"),
    )
    .unwrap();
    assert_eq!(selected, valid);
    fs::write(
        &valid,
        b"carrier_overrides:\n  '234/33': {custom_epdg: 'unicode-filename.invalid'}\n",
    )
    .unwrap();
    let value =
        preview_reader_status_with_path(
            &snapshot(Some(metadata())),
            &root,
            || Ok(selected.clone()),
        )
        .unwrap();
    assert_eq!(value["epdg_fqdn"], "unicode-filename.invalid");
    assert_eq!(value["carrier_config_path"], valid.to_str().unwrap());
    assert_eq!(
        serde_json::from_slice::<Value>(&serde_json::to_vec(&value).unwrap()).unwrap(),
        value
    );

    fs::write(&valid, b"carrier_overrides: [malformed").unwrap();
    let malformed =
        preview_reader_status_with_path(&snapshot(Some(metadata())), &root, || Ok(selected))
            .err()
            .unwrap();
    assert_eq!(malformed.code(), "native_carrier_overrides_invalid");
    assert!(serde_json::to_vec(&malformed.status_projection()).is_ok());

    // macOS filesystems reject names containing invalid UTF-8. Preserve the
    // raw CLI bytes for the attempted read and return a serializable error.
    let invalid = root.join(OsString::from_vec(b"carrier-\xff.yaml".to_vec()));
    let selected = resolve_path(
        crate::daemon_args::parse([
            OsString::from("-vowifi-config"),
            invalid.clone().into_os_string(),
        ])
        .unwrap()
        .vowifi_config
        .as_deref(),
        None,
        || panic!("absolute path needs no cwd"),
        || panic!("selected path needs no default"),
    )
    .unwrap();
    assert_eq!(selected, invalid);
    assert!(selected.to_str().is_none());
    // APFS can treat this raw path as missing on read even though creating it
    // fails with EILSEQ. Both supported filesystem outcomes must serialize.
    let projected = match preview_reader_status_with_path(
        &snapshot(Some(metadata())),
        &root,
        || Ok(selected),
    ) {
        Ok(value) => value,
        Err(failure) => {
            assert_eq!(failure.code(), "native_carrier_overrides_invalid");
            failure.status_projection()
        }
    };
    assert!(
        projected["carrier_config_path"]
            .as_str()
            .unwrap()
            .contains('\u{fffd}')
    );
    assert!(serde_json::to_vec(&projected).is_ok());
    fs::remove_dir_all(root).unwrap();
}
