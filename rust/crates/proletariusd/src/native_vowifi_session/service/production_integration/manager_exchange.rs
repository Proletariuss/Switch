//! Native manager -> real short card workflow -> IKE/IMS -> original N-wait store.
use super::*;
use crate::multistandby_native_runtime::{
    Action, ConnectionOptions, Inventory, Manager, ProfileSpec, Resources,
};
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_owns_real_networks_and_existing_nwait_sms_claims() {
    if std::env::var_os("SWITCH_NATIVE_MANAGER_496_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/native-manager-496-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","native_vowifi_session::service::production_integration::manager_exchange::manager_owns_real_networks_and_existing_nwait_sms_claims","--nocapture"]).env("SWITCH_NATIVE_MANAGER_496_CHILD",&root).env("SWITCH_DATA_DIR",&root).env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(55),async{
  let root=PathBuf::from(std::env::var_os("SWITCH_NATIVE_MANAGER_496_CHILD").unwrap());let physical=crate::multistandby_card::production::NativeFixture::new();physical.pool.shutdown().await.unwrap();
  let store=crate::multistandby_store::Store::open(&root).unwrap();let p1=physical.profile(true);let p2=physical.profile(false);
  let catalog=store.apply("catalog",serde_json::json!({"event_id":"manager-catalog","reader":"Workflow Reader","profiles":[{"eid":p1.eid,"iccid":p1.iccid,"display_name":"One","phone_number":""},{"eid":p2.eid,"iccid":p2.iccid,"display_name":"Two","phone_number":""}]})).unwrap();
  let profiles=catalog["profiles"].as_array().unwrap().iter().map(|p|ProfileSpec{profile_id:p["profile_id"].as_str().unwrap().into(),card_id:p["card_id"].as_str().unwrap().into(),eid:p["eid"].as_str().unwrap().into(),iccid:p["iccid"].as_str().unwrap().into(),aid:p1.aid.clone(),display_name:"Fixture".into(),phone_number:String::new(),physical_enabled:p["iccid"]==p2.iccid}).collect::<Vec<_>>();
  let first=profiles.iter().find(|p|p.iccid==p1.iccid).unwrap().profile_id.clone();let second=profiles.iter().find(|p|p.iccid==p2.iccid).unwrap().profile_id.clone();let card=profiles[0].card_id.clone();
  let(peer,mut sms)=ike_peer::Peer::with_multi_sms(root.clone(),store.clone(),first.clone()).await;
  let events=Arc::new(crate::safe_events::Hub::new());
  let manager=Manager::new_with_events(Resources{owner:physical.owner.clone(),gate:physical.gate.clone(),card_service:crate::multistandby_card::Service::new(physical.owner.clone(),physical.gate.clone(),None,None),data_root:root.clone(),store:store.clone(),journal:Arc::new(std::sync::Mutex::new(crate::multistandby_runtime_report::Journal::open(&root.join("runtime-report.json"),1))),transactions:Default::default(),connection:ConnectionOptions{proxy:peer.proxy.to_string(),epdg:"epdg.invalid".into(),epdg_port:Some(500)}},events.clone()).unwrap();
  manager.install_inventory(Inventory{reader:"Workflow Reader".into(),generation:1,profiles}).await.unwrap();
  drop(manager.execute(Action::Start,card.clone(),vec![first.clone()]).unwrap()); // Lost caller still leaves the retained command owned.
  ready(&manager,&first).await;assert_eq!(sms.next().await,sms_peer::Event::IncomingAccepted);assert_eq!(sms.next().await,sms_peer::Event::IncomingRpAck);
  manager.execute(Action::Start,card.clone(),vec![second.clone()]).unwrap().wait().await.unwrap();ready(&manager,&second).await;
  assert_eq!(physical.auth_count(),4);assert_eq!(physical.transaction_count(),0);assert!(manager.send_readiness(&first).channel_ready);
  assert!(!manager.retry_due(crate::multistandby_runtime_report::RetryDue{profile_id:first.clone(),owner_id:"stale".into(),generation:1,attempt_id:"stale".into()}));
  let queued=store.apply("queue",serde_json::json!({"event_id":"manager-queue","profile_id":first,"client_request_id":"manager-outgoing","phone":sms_peer::RECIPIENT,"body":sms_peer::OUTGOING})).unwrap();assert!(manager.maintenance());
  assert!(matches!(sms.next().await,sms_peer::Event::Submitted(_)));assert_eq!(sms.next().await,sms_peer::Event::SubmitRpAckAccepted);
  tokio::time::timeout(Duration::from_secs(5),async{loop{let rows:serde_json::Value=serde_json::from_reader(store.snapshot_file().unwrap().unwrap()).unwrap();if rows.to_string().contains("\"state\":\"accepted\""){assert!(rows.to_string().contains(queued["message_id"].as_str().unwrap()));break}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
  let generation=manager.snapshot(&card)["profiles"].as_array().unwrap().iter().find(|p|p["profile_id"]==first).unwrap()["session_generation"].as_u64().unwrap();
  peer.trace.stop_requested.store(true,Ordering::SeqCst); // Authorize expected client EOF; the peer still requires authenticated IKE DELETE.
  manager.execute(Action::Retry,card.clone(),vec![first.clone()]).unwrap().wait().await.unwrap();
  tokio::time::timeout(Duration::from_secs(15),async{loop{let snapshot=manager.snapshot(&card);let profile=snapshot["profiles"].as_array().unwrap().iter().find(|p|p["profile_id"]==first).unwrap();if profile["session_generation"].as_u64().unwrap()>generation&&manager.send_readiness(&first).channel_ready{break}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
  assert!(!manager.registration_due(crate::multistandby_runtime_report::RegistrationDue{profile_id:first.clone(),owner_id:manager.boot_id().into(),generation}));
  assert!(manager.send_readiness(&second).channel_ready);assert_eq!(physical.transaction_count(),0);assert_eq!(physical.auth_count(),6);
  manager.execute(Action::Stop,card.clone(),vec![first.clone()]).unwrap().wait().await.unwrap();assert!(!manager.send_readiness(&first).channel_ready);assert!(manager.send_readiness(&second).channel_ready);
  manager.execute(Action::Stop,card.clone(),vec![second.clone()]).unwrap().wait().await.unwrap();manager.shutdown().await.unwrap();assert_eq!(peer.trace.deletes.load(Ordering::SeqCst),3);assert_eq!(physical.transaction_count(),0);peer.finish().await;physical.finish().await;
  let events=diagnostic_events(&events).await;
  assert_attempts(&events,3);
  assert_eq!(events.iter().filter(|e|e.stage=="complete"&&e.code=="manual_retry").count(),1);
  assert_eq!(events.iter().filter(|e|e.stage=="complete"&&e.code=="cancelled").count(),2);
  assert_eq!(events.iter().filter(|e|e.stage=="ready"&&e.code=="ready").count(),3,"one event per actual ready transition, maintenance does not repeat it");
  let encoded=serde_json::to_string(&events).unwrap();
  for secret in [&first,&second,&card,&p1.eid,&p1.iccid,&p2.iccid,sms_peer::RECIPIENT] { assert!(!encoded.contains(secret),"diagnostic identity leak"); }

 }).await.unwrap();
}
async fn ready(manager: &Manager, id: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if manager.send_readiness(id).channel_ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn diagnostic_events(hub: &crate::safe_events::Hub) -> Vec<crate::safe_events::Event> {
    use http_body_util::BodyExt;
    let body = crate::safe_events::handle_public(hub)
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    serde_json::from_value(value["events"].clone()).unwrap()
}
fn assert_attempts(events: &[crate::safe_events::Event], count: usize) {
    let prepared = events
        .iter()
        .filter(|e| e.stage == "prepare")
        .collect::<Vec<_>>();
    assert_eq!(prepared.len(), count);
    let ids = prepared
        .iter()
        .map(|e| e.correlation_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        ids.len(),
        count,
        "attempt IDs must not be reused across profile or retry"
    );
    for first in prepared {
        assert_eq!(first.generation, 0);
        let attempt = events
            .iter()
            .filter(|e| e.correlation_id == first.correlation_id)
            .collect::<Vec<_>>();
        let connections = attempt
            .iter()
            .filter(|e| e.stage == "connect")
            .collect::<Vec<_>>();
        assert!(connections.len() <= 1);
        if let Some(connect) = connections.first() {
            assert!(connect.generation > 0);
            assert!(
                attempt
                    .iter()
                    .skip(1)
                    .all(|e| e.generation == connect.generation)
            );
        } else {
            assert!(attempt.iter().all(|e| e.generation == 0));
        }
        assert_eq!(attempt.iter().filter(|e| e.stage == "retire").count(), 1);
        assert_eq!(attempt.iter().filter(|e| e.stage == "complete").count(), 1);
    }
}
mod recovery;
