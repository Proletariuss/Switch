//! Two actual protected network sessions share one FIFO physical card owner.
use super::*;
use crate::native_registered_session::Control;
use crate::native_sms_originating::{Phase, RpResult, TextSend};
use crate::native_vowifi_session::multi::{NativeMultiSession, Runtime};
use tokio::sync::{oneshot, watch};
async fn ready(session: &NativeMultiSession) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let s = session.snapshot();
            assert!(s.termination.is_none(), "startup: {:?}", s.termination);
            if s.phase == ordinary::Phase::Registered {
                assert!(s.sms_ready);
                assert!(!s.call_ready);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_multi_two_profiles_share_short_card_jobs_and_real_ike_ims_sms() {
    if std::env::var_os("SWITCH_NATIVE_MULTI_495_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/native-multi-495-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move ||std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","native_vowifi_session::service::production_integration::native_multi_exchange::native_multi_two_profiles_share_short_card_jobs_and_real_ike_ims_sms","--nocapture"])
            .env("SWITCH_NATIVE_MULTI_495_CHILD",&root).env("SWITCH_DATA_DIR",&root).env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(45), async {
        let root = PathBuf::from(std::env::var_os("SWITCH_NATIVE_MULTI_495_CHILD").unwrap());
        let physical = crate::multistandby_card::production::NativeFixture::new();
        let runtime = Runtime::new(
            physical.pool.clone(),
            physical.gate.clone(),
            root.clone(),
            Default::default(),
        )
        .unwrap();
        let store = crate::multistandby_store::Store::open(&root).unwrap();
        store
            .apply(
                "catalog",
                serde_json::json!({"event_id":"catalog-native-multi", "profiles":[
                    {"profile_id":"profile-first", "card_id":"card-shared"},
                    {"profile_id":"profile-second", "card_id":"card-shared"}
                ]}),
            )
            .unwrap();
        let (peer, mut sms) =
            ike_peer::Peer::with_multi_sms(root.clone(), store.clone(), "profile-first".into())
                .await;
        let peer2 = ike_peer::Peer::new().await;
        let (stop, _) = watch::channel(false);
        let plan = |proxy| PlanRequest {
            reader: "Workflow Reader".into(),
            proxy,
            epdg: "epdg.invalid".into(),
            epdg_port: Some(500),
        };
        let first = runtime
            .start_for_profile(
                physical.profile(true),
                plan(peer.proxy.to_string()),
                stop.subscribe(),
                store.clone(),
                "profile-first".into(),
            )
            .await
            .unwrap();
        ready(&first).await;
        assert_eq!(physical.auth_count(), 2);
        assert_eq!(physical.transaction_count(), 0);
        assert_eq!(sms.next().await, sms_peer::Event::IncomingAccepted);
        assert_eq!(sms.next().await, sms_peer::Event::IncomingRpAck);
        sms.commands
            .send(sms_peer::Command::Duplicate)
            .await
            .unwrap();
        assert_eq!(sms.next().await, sms_peer::Event::IncomingAccepted);
        assert_eq!(sms.next().await, sms_peer::Event::IncomingRpAck);
        assert!(
            !root.join("sms-archive.json").exists(),
            "Nwait cannot write ordinary inbox"
        );
        let (inbox, _) = store.forwarding_page(0).unwrap();
        assert_eq!(inbox.len(), 1);
        assert!(inbox[0].id().starts_with("nwait:profile-first:"));
        let second = runtime
            .start(
                physical.profile(false),
                plan(peer2.proxy.to_string()),
                stop.subscribe(),
            )
            .await
            .unwrap();
        ready(&second).await;
        assert_eq!(physical.auth_count(), 4);
        assert_eq!(physical.transaction_count(), 0);
        assert!(first.snapshot().sms_ready);
        assert!(second.snapshot().sms_ready);
        assert_ne!(first.generation(), second.generation());
        assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 1);
        assert_eq!(peer2.trace.registers.load(Ordering::SeqCst), 1);
        let (reply, wait) = oneshot::channel();
        first
            .controls()
            .send(Control::SendText {
                input: TextSend {
                    generation: first.generation(),
                    batch_id: "native-multi-real-outgoing".into(),
                    recipient: sms_peer::RECIPIENT.to_owned().into(),
                    text: sms_peer::OUTGOING.to_owned().into(),
                    request_status_report: true,
                },
                reply,
            })
            .await
            .unwrap();
        wait.await.unwrap().unwrap();
        assert!(matches!(sms.next().await, sms_peer::Event::Submitted(_)));
        assert_eq!(sms.next().await, sms_peer::Event::SubmitRpAckAccepted);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (reply, wait) = oneshot::channel();
                first
                    .controls()
                    .send(Control::SmsStatus {
                        generation: first.generation(),
                        batch_id: "native-multi-real-outgoing".into(),
                        reply,
                    })
                    .await
                    .unwrap();
                let receipt = wait.await.unwrap().unwrap().unwrap();
                if receipt.phase == Phase::Accepted {
                    assert_eq!(receipt.rp_results, vec![(1, RpResult::Ack)]);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let (reply, wait) = oneshot::channel();
        first
            .controls()
            .send(Control::SmsStatus {
                generation: second.generation(),
                batch_id: "native-multi-real-outgoing".into(),
                reply,
            })
            .await
            .unwrap();
        assert!(
            wait.await.unwrap().is_err(),
            "different profile generation cannot inspect first profile SMS"
        );
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        // A real durability failure must retire this consumer, preserving the
        // natural failure and sibling registration. No successful SIP ACK is injected.
        std::fs::write(
            root.join("multistandby/store-v1.json"),
            b"not a valid archive",
        )
        .unwrap();
        sms.commands
            .send(sms_peer::Command::Duplicate)
            .await
            .unwrap();
        let exit = first.wait_exit().await;
        assert!(exit.joined && exit.cleanup_confirmed);
        assert!(exit.snapshot.termination.is_some());
        assert_ne!(
            exit.snapshot.termination.as_ref().unwrap().reason,
            ordinary::status::EndReason::UserStop
        );

        assert!(
            second.snapshot().sms_ready,
            "single-profile release must preserve sibling"
        );
        peer2.trace.stop_requested.store(true, Ordering::SeqCst);
        second.finish().await.unwrap();
        runtime.shutdown().await.unwrap();
        assert!(first.controls().is_closed());
        assert!(second.controls().is_closed());
        let t1 = peer.trace.clone();
        let t2 = peer2.trace.clone();
        peer.finish().await;
        peer2.finish().await;
        assert_eq!(t1.deletes.load(Ordering::SeqCst), 1);
        assert_eq!(t2.deletes.load(Ordering::SeqCst), 1);
        physical.finish().await;
        assert!(!root.join("go.sock").exists());
        // Stop during the real IKE EAP-AKA card callback. Session publication
        // does not relinquish the physical job and stop must wait for its join.
        let physical = crate::multistandby_card::production::NativeFixture::new();
        let runtime = Runtime::new(
            physical.pool.clone(),
            physical.gate.clone(),
            root.join("aka-stop"),
            Default::default(),
        )
        .unwrap();
        let release = physical.hold_auth();
        let peer = ike_peer::Peer::new().await;
        let session = runtime
            .start(
                physical.profile(true),
                plan(peer.proxy.to_string()),
                stop.subscribe(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while physical.auth_count() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        session.request_stop();
        let exit = session.wait_exit();
        tokio::pin!(exit);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut exit)
                .await
                .is_err()
        );
        assert_eq!(physical.transaction_count(), 1);
        release.send(()).unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(5), &mut exit)
            .await
            .unwrap();
        assert!(exit.joined && exit.cleanup_confirmed);
        assert_eq!(physical.transaction_count(), 0);
        runtime.shutdown().await.unwrap();
        peer.finish().await;
        physical.finish().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_unpublished_start_retains_physical_job_until_join_before_pool_release() {
    let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/native-multi-cancel-496-{}",
        crate::multistandby_store::random_token().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let physical = crate::multistandby_card::production::NativeFixture::new();
    let runtime = Runtime::new(
        physical.pool.clone(),
        physical.gate.clone(),
        root,
        Default::default(),
    )
    .unwrap();
    let (release, entered) = physical.hold_next_apdu();
    let (stop, _) = watch::channel(false);
    let startup = runtime
        .begin_start(
            physical.profile(true),
            PlanRequest {
                reader: "Workflow Reader".into(),
                proxy: "127.0.0.1:9".into(),
                epdg: "epdg.invalid".into(),
                epdg_port: Some(500),
            },
            stop.subscribe(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // The public waiter disappears during a real retained APDU. Neither a
    // cancelled future nor Runtime shutdown is permission to restore/release.
    drop(startup);
    let joined = runtime.shutdown();
    tokio::pin!(joined);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut joined)
            .await
            .is_err()
    );
    assert_eq!(physical.transaction_count(), 1);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), &mut joined)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(physical.transaction_count(), 0);
    physical.finish().await;
}
