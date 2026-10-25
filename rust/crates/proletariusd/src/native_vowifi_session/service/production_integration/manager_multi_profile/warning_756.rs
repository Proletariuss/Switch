use super::super::facade_exchange::{public_http, resources};
use super::*;
use crate::multistandby_native_service::Service as Facade;

async fn send(oracle: &Oracle, profile: Profile, serial: u32, tpdu: &[u8], reject_report: bool) {
    let mut rpdu = vec![1, 42, 0, 0, u8::try_from(tpdu.len()).unwrap()];
    rpdu.extend_from_slice(tpdu);
    let sender = oracle.commands[profile.index()]
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    sender
        .send(Command::WarningSms {
            serial,
            rpdu,
            reject_report,
        })
        .await
        .unwrap();
}
fn raw_count(store: &crate::multistandby_store::Store) -> usize {
    let value: serde_json::Value =
        serde_json::from_reader(store.snapshot_file().unwrap().unwrap()).unwrap();
    fn count(v: &serde_json::Value) -> usize {
        match v {
            serde_json::Value::Object(m) => {
                usize::from(m.get("kind").and_then(serde_json::Value::as_str) == Some("undecoded"))
                    + m.values().map(count).sum::<usize>()
            }
            serde_json::Value::Array(a) => a.iter().map(count).sum(),
            _ => 0,
        }
    }
    count(&value)
}
async fn profiles(
    facade: &Arc<Facade>,
    store: &crate::multistandby_store::Store,
    operations: &crate::multistandby_operations::Store,
) -> serde_json::Value {
    let (status, value) = public_http(
        facade.clone(),
        store.clone(),
        operations.clone(),
        "GET",
        "/api/multistandby/profiles",
        json!({}),
    )
    .await;
    assert_eq!(status, hyper::StatusCode::OK);
    value
}
fn public_row<'a>(value: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    value["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["profile_id"] == id)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nwait_warning_crosses_real_distinct_sessions_and_public_profiles_without_degrading_lines()
{
    const CHILD: &str = "SWITCH_NWAIT_WARNING_756_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/nwait-warning-756-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_vowifi_session::service::production_integration::manager_multi_profile::warning_756::nwait_warning_crosses_real_distinct_sessions_and_public_profiles_without_degrading_lines", "--nocapture"])
            .env(CHILD, &root).env("SWITCH_DATA_DIR", &root).env("PROLETARIUS_VOWIFI_CONFIG", config)
            .kill_on_drop(true).output().await.unwrap();
        assert!(
            output.status.success(),
            "synthetic child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(45), async {
        let root = PathBuf::from(std::env::var_os(CHILD).unwrap());
        let physical =
            crate::multistandby_card::production::NativeFixture::with_distinct_profiles();
        physical.pool.shutdown().await.unwrap();
        let oracle = Oracle::new();
        let peer = ike_peer::Peer::with_multi_profile_oracle(oracle.clone()).await;
        let facade = Facade::new(resources(
            &physical,
            &root,
            ConnectionOptions {
                proxy: peer.proxy.to_string(),
                epdg: "epdg.invalid".into(),
                epdg_port: Some(500),
            },
        ))
        .unwrap();
        assert_eq!(
            facade.snapshot("", true, false).await.unwrap()["available"],
            true
        );
        let inventory = facade.snapshot("", false, false).await.unwrap();
        let entries = inventory["profiles"].as_array().unwrap();
        let first = entries
            .iter()
            .find(|p| p["iccid"] == Profile::A.iccid())
            .unwrap()["profile_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = entries
            .iter()
            .find(|p| p["iccid"] == Profile::B.iccid())
            .unwrap()["profile_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let card = entries[0]["card_id"].as_str().unwrap().to_owned();
        let store = crate::multistandby_store::Store::open(&root).unwrap();
        let operations = crate::multistandby_operations::Store::open(&root).unwrap();
        operations.recover().unwrap();
        facade
            .execute(
                Action::Start,
                card.clone(),
                vec![first.clone(), second.clone()],
            )
            .await
            .unwrap();
        wait_for(
            || facade.ready(&first).channel_ready && facade.ready(&second).channel_ready,
            20,
            "both warning test profiles ready",
        )
        .await;
        let before = profiles(&facade, &store, &operations).await;
        assert_eq!(public_row(&before, &first)["last_error"], "");
        assert_eq!(public_row(&before, &second)["last_error"], "");
        let warning =
            crate::ims_sms_transactions::NativeSmsArchiveWarning::RawRecordUnmerged.message();
        // Unknown TPDU, with a real committed archive and a real rejected RP-ACK
        // transaction. Rejection must not erase the already committed warning.
        send(&oracle, Profile::A, 756, &[3], true).await;
        wait_for(
            || {
                oracle
                    .facts(Profile::A)
                    .warning_rp_acks
                    .load(Ordering::SeqCst)
                    == 1
            },
            5,
            "rejected warning report",
        )
        .await;
        let after = profiles(&facade, &store, &operations).await;
        assert_eq!(public_row(&after, &first)["last_error"], warning);
        assert_eq!(public_row(&after, &second)["last_error"], "");
        assert_eq!(raw_count(&store), 1);
        assert!(store.forwarding_page(0).unwrap().0.is_empty());
        // Identical RPDU retransmission is independently ACKed, with no duplicate
        // raw record or new generation and no health/failure/retry mutation.
        send(&oracle, Profile::A, 757, &[3], false).await;
        wait_for(
            || {
                oracle
                    .facts(Profile::A)
                    .warning_rp_acks
                    .load(Ordering::SeqCst)
                    == 2
            },
            5,
            "duplicate report",
        )
        .await;
        assert_eq!(raw_count(&store), 1);
        // Decoded text fragments collide at the same concatenation index. The
        // warning follows the store's isolation decision, not decoded=false.
        let part = |text| {
            vec![
                0x40, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 8, 5, 0, 3, 7, 2, 1,
                0, text,
            ]
        };
        send(&oracle, Profile::A, 758, &part(0x41), false).await;
        wait_for(
            || {
                oracle
                    .facts(Profile::A)
                    .warning_rp_acks
                    .load(Ordering::SeqCst)
                    == 3
            },
            5,
            "partial fragment",
        )
        .await;
        send(&oracle, Profile::A, 759, &part(0x42), false).await;
        wait_for(
            || {
                oracle
                    .facts(Profile::A)
                    .warning_rp_acks
                    .load(Ordering::SeqCst)
                    == 4
            },
            5,
            "ambiguous fragment",
        )
        .await;
        assert_eq!(raw_count(&store), 2);
        assert!(store.forwarding_page(0).unwrap().0.is_empty());
        let final_profiles = profiles(&facade, &store, &operations).await;
        for id in [&first, &second] {
            let old = public_row(&before, id);
            let current = public_row(&final_profiles, id);
            for key in [
                "session_generation",
                "consecutive_failures",
                "desired_state",
                "sms_ready",
                "health",
            ] {
                assert_eq!(current[key], old[key], "warning changed runtime fact {key}");
            }
            assert_eq!(current["sms_ready"], true);
            assert_eq!(current["health"], "ready");
        }
        assert_eq!(public_row(&final_profiles, &first)["last_error"], warning);
        assert_eq!(public_row(&final_profiles, &second)["last_error"], "");
        assert_eq!(
            oracle
                .facts(Profile::A)
                .warning_accepted
                .load(Ordering::SeqCst),
            4
        );
        assert_eq!(
            oracle
                .facts(Profile::B)
                .warning_accepted
                .load(Ordering::SeqCst),
            0
        );
        assert!(!final_profiles.to_string().contains("warning-756-"));
        assert!(!final_profiles.to_string().contains("raw_rpdu"));
        for p in [Profile::A, Profile::B] {
            oracle.facts(p).stop_requested.store(true, Ordering::SeqCst);
        }
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        facade
            .execute(Action::Stop, card, vec![first.clone(), second.clone()])
            .await
            .unwrap();
        let stopped = profiles(&facade, &store, &operations).await;
        assert_ne!(public_row(&stopped, &first)["last_error"], warning);
        facade.shutdown().await.unwrap();
        peer.finish().await;
        physical.finish().await;
    })
    .await
    .expect("warning integration deadline");
}
