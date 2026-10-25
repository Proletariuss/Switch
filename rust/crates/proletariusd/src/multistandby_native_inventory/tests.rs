use super::*;
use crate::multistandby_card::production::NativeFixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inventory_reads_real_workflow_without_switch_or_authentication() {
    let fixture = NativeFixture::new();
    fixture.pool.shutdown().await.unwrap();
    let (owner, gate, service) = fixture.inventory_resources();
    let result = read(owner, gate.clone(), service, "Workflow Reader".into(), 1)
        .await
        .unwrap();
    assert_eq!(result["reader"], "Workflow Reader");
    assert_eq!(result["reader_generation"], 1);
    let groups = result["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["eid"], fixture.profile(true).eid);
    assert_eq!(groups[0]["profiles"].as_array().unwrap().len(), 2);
    assert_eq!(fixture.auth_count(), 0);
    assert_eq!(fixture.switch_count(), 0);
    assert_eq!(fixture.transaction_count(), 0);
    let parent = gate.acquire_native_multi("Workflow Reader", 1).unwrap();
    gate.release_native_multi(&parent).unwrap();
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_inventory_never_starts_a_card_transaction() {
    let fixture = NativeFixture::new();
    fixture.pool.shutdown().await.unwrap();
    let (owner, gate, service) = fixture.inventory_resources();
    assert!(
        read(owner, gate, service, "Workflow Reader".into(), 2)
            .await
            .is_err()
    );
    assert_eq!(fixture.transaction_count(), 0);
    assert_eq!(fixture.auth_count(), 0);
    assert_eq!(fixture.switch_count(), 0);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_waiter_keeps_parent_until_actual_apdu_completion() {
    let fixture = NativeFixture::new();
    fixture.pool.shutdown().await.unwrap();
    let (owner, gate, service) = fixture.inventory_resources();
    let (unblock, entered) = fixture.hold_next_apdu();
    let task = tokio::spawn(read(
        owner,
        gate.clone(),
        service,
        "Workflow Reader".into(),
        1,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(gate.acquire_native_multi("Workflow Reader", 1).is_err());
    unblock.send(()).unwrap();
    let parent = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(parent) = gate.acquire_native_multi("Workflow Reader", 1) {
                break parent;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    gate.release_native_multi(&parent).unwrap();
    assert_eq!(fixture.transaction_count(), 0);
    assert_eq!(fixture.auth_count(), 0);
    assert_eq!(fixture.switch_count(), 0);
    fixture.finish().await;
}

#[test]
fn inventory_query_rejects_unrecognized_authority_fields() {
    assert!(
        serde_json::from_value::<Query>(json!({"reader":"r","reader_generation":1,"extra":true}))
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_inventory_releases_real_transaction_and_parent() {
    let fixture = NativeFixture::new();
    fixture.pool.shutdown().await.unwrap();
    let (owner, gate, service) = fixture.inventory_resources();
    fixture.fail_next_apdu();
    assert!(
        read(owner, gate.clone(), service, "Workflow Reader".into(), 1)
            .await
            .is_err()
    );
    assert_eq!(fixture.transaction_count(), 0);
    let parent = gate.acquire_native_multi("Workflow Reader", 1).unwrap();
    gate.release_native_multi(&parent).unwrap();
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_exact_loss_receipt_releases_inventory_even_after_caller_cancel() {
    use crate::reader_reservation::{NativeAuthority, NativeReaderFacts, NativeTransitionKind};
    let fixture = NativeFixture::new();
    fixture.pool.shutdown().await.unwrap();
    let (owner, gate, service) = fixture.inventory_resources();
    let physical = fixture.physical_binding();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (proceed, paused) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(read_retained(
        owner.clone(),
        gate.clone(),
        service,
        physical.reader.clone(),
        1,
        Some(BeforeRelease {
            entered,
            proceed: paused,
        }),
    ));
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.transaction_count(),
        0,
        "inventory has really joined before the release race"
    );
    let closing_owner = owner.clone();
    let closing_binding = physical.clone();
    assert!(tokio::task::spawn_blocking(move||closing_owner.test_close_for_presence(&closing_binding)).await.unwrap().unwrap());
    let loss = gate
        .prepare_native_transition(
            &physical.producer,
            &gate.native_reader_snapshot().0,
            NativeTransitionKind::Loss,
            &physical.reader,
            NativeAuthority::Loss(physical.clone()),
        )
        .unwrap();
    // Publish logical loss first. Even after a real physical close, only the
    // separately pinned exact receipt is authority to retire this parent.
    gate.commit_native_transition(&loss, None, NativeReaderFacts::default())
        .unwrap();
    proceed.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !task.is_finished(),
        "logical generation change must not complete release"
    );
    assert!(
        gate.prepare_native_transition(
            &physical.producer,
            &gate.native_reader_snapshot().0,
            NativeTransitionKind::Connect,
            &physical.reader,
            NativeAuthority::Idle
        )
        .is_err()
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    gate.commit_confirmed_native_presence_loss(&owner, &loss)
        .unwrap();
    owner.test_ack_presence_loss(&physical);
    // The dropped outer read future must not have dropped its retained parent.
    // Once exact proof arrives, its worker releases the old owner, so a fresh
    // lifecycle transition and generation can really acquire the card.
    let connect = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(transition) = gate.prepare_native_transition(
                &physical.producer,
                &gate.native_reader_snapshot().0,
                NativeTransitionKind::Connect,
                &physical.reader,
                NativeAuthority::Idle,
            ) {
                break transition;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let replacement = owner.native_open(&physical.reader).unwrap();
    gate.commit_native_transition(&connect, Some(&replacement), NativeReaderFacts::default())
        .unwrap();
    let next = gate.acquire_native_multi(&physical.reader, 3).unwrap();
    gate.release_native_multi(&next).unwrap();
    assert_eq!(fixture.auth_count(), 0);
    assert_eq!(fixture.switch_count(), 0);
    owner.native_close(&replacement).unwrap();
    owner.shutdown().await.unwrap();
}
