use super::*;
use crate::{
    reader_ble_apdu::BleApduTransport,
    reader_pcsc::{CardWorkflow, ResetAction},
    reader_reservation::NativeReaderFacts,
};
use proletarius_sim::{AkaResult, ApduTransport};
use std::{
    fmt::Display,
    time::{Duration, Instant},
};

struct Control<'a> {
    inner: &'a Inner,
    request: &'a Submit,
    cancelled: Arc<AtomicBool>,
    recovery: Option<Instant>,
}
impl Control<'_> {
    fn fresh(&self) -> Result<(), Failure> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Failure::new("multistandby_cancelled", "卡片任务已取消"));
        }
        self.inner
            .gate
            .validate_card_workflow(
                &self.request.transaction,
                &self.request.reader,
                self.request.reader_generation,
            )
            .map_err(|e| Failure::new(e, "卡片任务归属已失效"))
    }
    fn io(&self) -> Result<(), Failure> {
        if let Some(deadline) = self.recovery {
            if Instant::now() < deadline {
                Ok(())
            } else {
                Err(Failure::new(
                    "multistandby_recovery_expired",
                    "切卡恢复期限已结束",
                ))
            }
        } else {
            self.fresh()
        }
    }
}
struct StringTransport<'a, T>(&'a mut T);
impl<T: ApduTransport> ApduTransport for StringTransport<'_, T>
where
    T::Error: Display,
{
    type Error = String;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, String> {
        self.0.transmit(command).map_err(|e| e.to_string())
    }
}
struct Guarded<'a> {
    transport: &'a mut dyn ApduTransport<Error = String>,
    control: &'a Control<'a>,
}
impl ApduTransport for Guarded<'_> {
    type Error = String;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, String> {
        self.control.io().map_err(|e| e.to_string())?;
        self.transport.transmit(command)
    }
}
enum Physical {
    Pcsc(CardWorkflow),
    Ble(BleApduTransport),
}
impl Physical {
    fn with<R>(
        &mut self,
        control: &Control<'_>,
        operation: impl FnOnce(&mut Guarded<'_>) -> Result<R, Failure>,
    ) -> Result<R, Failure> {
        control.io()?;
        match self {
            Self::Pcsc(card) => card
                .with_transaction(|transport| {
                    let mut transport = StringTransport(transport);
                    operation(&mut Guarded {
                        transport: &mut transport,
                        control,
                    })
                })
                .map_err(Failure::physical)?,
            Self::Ble(transport) => {
                let mut transport = StringTransport(transport);
                operation(&mut Guarded {
                    transport: &mut transport,
                    control,
                })
            }
        }
    }
    fn recovery_started(&self) -> Result<(), Failure> {
        if let Self::Pcsc(card) = self {
            card.recovery_started(Duration::from_secs(45))
                .map_err(Failure::physical)?;
        }
        Ok(())
    }
    fn reset(&mut self, control: &Control<'_>, power: bool) -> Result<(), Failure> {
        control.io()?;
        std::thread::sleep(Duration::from_millis(500));
        control.io()?;
        match self {
            Self::Pcsc(card) => {
                card.reset(ResetAction::Reopen).map_err(Failure::physical)?;
                if power
                    && control
                        .request
                        .reader
                        .to_ascii_lowercase()
                        .contains("estkme")
                {
                    control.io()?;
                    card.reset(ResetAction::Reset).map_err(Failure::physical)?;
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            Self::Ble(transport) => {
                if power {
                    transport
                        .reset()
                        .map_err(|e| Failure::new("reader_reset_unconfirmed", e))?;
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        Ok(())
    }
}
trait Card {
    fn fresh(&mut self) -> Result<(), Failure>;
    fn verify(&mut self) -> Result<crate::reader_esim_download::EuiccFacts, Failure>;
    fn read(&mut self) -> Result<Value, Failure>;
    fn reset(&mut self, power: bool) -> Result<(), Failure>;
    fn enable(&mut self) -> Result<(), Failure>;
    fn recovery(&mut self) -> Result<(), Failure>;
    fn notifications(
        &mut self,
        facts: &crate::reader_esim_download::EuiccFacts,
    ) -> Result<(), Failure>;
    fn authenticate(&mut self) -> Result<Value, Failure>;
    fn publish(
        &mut self,
        identity: &Value,
        facts: crate::reader_esim_download::EuiccFacts,
    ) -> Result<(), Failure>;
}
struct Production<'a> {
    physical: Physical,
    control: Control<'a>,
    aid: Option<Vec<u8>>,
    recovery_armed: Option<Instant>,
}
impl Card for Production<'_> {
    fn fresh(&mut self) -> Result<(), Failure> {
        self.control.fresh()?;
        if let Physical::Pcsc(card) = &self.physical {
            card.checkpoint().map_err(Failure::physical)?;
        }
        Ok(())
    }
    fn verify(&mut self) -> Result<crate::reader_esim_download::EuiccFacts, Failure> {
        self.physical.with(&self.control, |t| {
            crate::reader_esim_download::multistandby_euicc(
                t,
                &self.control.request.eid,
                self.aid.as_deref(),
            )
            .map_err(|e| {
                Failure::new(
                    if e == "multistandby_identity_changed" {
                        "multistandby_identity_changed"
                    } else {
                        "reader_metadata_failed"
                    },
                    e,
                )
            })
        })
    }
    fn read(&mut self) -> Result<Value, Failure> {
        self.physical.with(&self.control, |t| {
            let metadata = proletarius_reader::read_sim_metadata_auto_with_transport(t)
                .map_err(|e| Failure::new("reader_metadata_failed", e.to_string()))?;
            crate::reader_lifecycle::metadata_value(metadata)
                .map_err(|e| Failure::new(e.code, e.message))
        })
    }
    fn reset(&mut self, power: bool) -> Result<(), Failure> {
        self.physical.reset(&self.control, power)
    }
    fn enable(&mut self) -> Result<(), Failure> {
        self.fresh()?;
        // ENABLE may take effect even when its reply or later notification
        // fails. Do not leave the previous profile advertised as live metadata.
        let mut facts = self
            .control
            .inner
            .gate
            .native_reader_snapshot()
            .1
            .unwrap_or_default();
        facts.metadata = None;
        self.control
            .inner
            .gate
            .publish_multistandby_metadata(
                &self.control.request.transaction,
                &self.control.request.reader,
                self.control.request.reader_generation,
                facts,
            )
            .map_err(|e| Failure::new(e, "卡片身份归属已失效"))?;
        self.physical.recovery_started()?;
        self.recovery_armed = Some(Instant::now() + Duration::from_secs(45));
        // While ENABLE is running, cancellation and principal checks still
        // apply to each fresh APDU. Retention is armed before submission.
        self.physical.with(&self.control, |t| {
            crate::reader_esim_download::multistandby_enable(
                t,
                &normalize_iccid(&self.control.request.iccid),
                self.aid.as_deref(),
                self.control.cancelled.clone(),
            )
            .map_err(|e| Failure::new("profile_switch_failed", e))
        })
    }
    fn recovery(&mut self) -> Result<(), Failure> {
        let Some(deadline) = self.recovery_armed else {
            return self.fresh().and(Err(Failure::new(
                "profile_switch_unconfirmed",
                "切卡未提交",
            )));
        };
        self.control.recovery = Some(deadline);
        Ok(())
    }
    fn notifications(
        &mut self,
        facts: &crate::reader_esim_download::EuiccFacts,
    ) -> Result<(), Failure> {
        let aids = self
            .aid
            .clone()
            .map(|v| vec![v])
            .unwrap_or_else(|| facts.notification_aids.clone());
        self.physical.with(&self.control, |t| {
            crate::reader_esim_download::process_lifecycle_notifications(t, &aids)
                .map_err(|e| Failure::new("switch_notification_pending", e))
        })
    }
    fn authenticate(&mut self) -> Result<Value, Failure> {
        self.fresh()?;
        // Reset the APDU guard to fresh admission. Recovery never authorizes a
        // new AKA even if the physical profile was successfully restored.
        self.control.recovery = None;
        self.physical.with(&self.control, |t| {
            let r = self.control.request;
            let app = if r.application == "isim" {
                proletarius_reader::AkaApplication::Isim
            } else {
                proletarius_reader::AkaApplication::Usim
            };
            let result = proletarius_reader::authenticate_bound_with_transport(
                t,
                app,
                &normalize_iccid(&r.iccid),
                &r.expected_imsi,
                &r.rand.unwrap(),
                &r.autn.unwrap(),
            )
            .map_err(|e| Failure::new("multistandby_authentication_failed", e.to_string()))?;
            Ok(match result {
                AkaResult::Success { res, ck, ik } => {
                    json!({"outcome":"success","res":res,"ck":ck,"ik":ik})
                }
                AkaResult::SynchronizationFailure { auts } => {
                    json!({"outcome":"synchronization_failure","auts":auts})
                }
                AkaResult::AuthenticationReject => json!({"outcome":"authentication_rejected"}),
            })
        })
    }
    fn publish(
        &mut self,
        identity: &Value,
        facts: crate::reader_esim_download::EuiccFacts,
    ) -> Result<(), Failure> {
        self.fresh()?;
        self.control
            .inner
            .gate
            .publish_multistandby_metadata(
                &self.control.request.transaction,
                &self.control.request.reader,
                self.control.request.reader_generation,
                NativeReaderFacts {
                    metadata: Some(identity.clone()),
                    has_esim: facts.available,
                    notification_aids: facts
                        .notification_aids
                        .iter()
                        .map(|aid| crate::reader_esim_download::encode_hex_lower(aid))
                        .collect(),
                    profile_name: facts.profile_name,
                    ble_connection: None,
                },
            )
            .map_err(|e| Failure::new(e, "卡片身份归属已失效"))
    }
}
pub(super) fn execute(
    inner: &Inner,
    request: &Submit,
    cancelled: Arc<AtomicBool>,
) -> Result<Value, Failure> {
    let control = Control {
        inner,
        request,
        cancelled,
        recovery: None,
    };
    control.fresh()?;
    let mut physical = if let Some(binding) = &request.pcsc {
        Physical::Pcsc(
            inner
                .owner
                .retain_card_workflow(binding)
                .map_err(Failure::physical)?,
        )
    } else {
        let socket = inner
            .ble_socket
            .as_deref()
            .ok_or_else(|| Failure::new("ble_bridge_unavailable", "蓝牙读卡器桥未配置"))?;
        let permit = crate::reader_ble_apdu::acquire_io_permit(socket)
            .map_err(|e| Failure::new("reader_busy", e))?;
        Physical::Ble(
            BleApduTransport::connect_reserved_with_gate(
                socket,
                inner.app_token.as_deref(),
                &request.reader,
                permit,
                inner.gate.clone(),
            )
            .map_err(|e| Failure::new("ble_reader_unavailable", e))?,
        )
    };
    if request.action == Action::Inventory {
        let result = physical.with(&control, |transport| {
            crate::reader_esim_download::multistandby_inventory(transport)
                .map_err(|e| Failure::new("reader_inventory_failed", e))
        })?;
        control.fresh()?;
        return Ok(result);
    }
    let aid = if request.aid.is_empty() {
        None
    } else {
        Some(
            (0..request.aid.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&request.aid[i..i + 2], 16).unwrap())
                .collect(),
        )
    };
    run(
        &mut Production {
            physical,
            control,
            aid,
            recovery_armed: None,
        },
        request,
    )
}
fn identity_ok(identity: &Value, iccid: Option<&str>) -> bool {
    identity["imsi"]
        .as_str()
        .is_some_and(|v| !v.trim().is_empty())
        && identity["iccid"].as_str().is_some_and(|v| {
            !v.is_empty()
                && iccid.is_none_or(|expected| normalize_iccid(v) == normalize_iccid(expected))
        })
}
fn run(card: &mut impl Card, request: &Submit) -> Result<Value, Failure> {
    card.fresh()?;
    if request.action == Action::Identity {
        card.verify()?;
        let identity = card.read()?;
        if !identity_ok(&identity, None) {
            return Err(Failure::new(
                "reader_metadata_failed",
                "当前 Profile 身份无法确认",
            ));
        }
        let facts = card.verify()?;
        card.publish(&identity, facts)?;
        return Ok(json!({"identity":identity}));
    }
    match card.verify() {
        Ok(_) => {}
        Err(e) if e.code == "multistandby_identity_changed" => return Err(e),
        Err(_) => {
            card.fresh()?;
            card.reset(false)?;
            card.verify()?;
        }
    }
    let mut identity = match card.read() {
        Ok(v) => v,
        Err(_) => {
            card.fresh()?;
            card.reset(false)?;
            card.verify()?;
            card.read()?
        }
    };
    if !identity_ok(&identity, Some(&request.iccid)) {
        let switched = card.enable();
        // A lost ENABLE response may still have changed the profile. Never
        // replay it: recover the retained handle and verify physical identity.
        card.recovery()?;
        card.reset(true)?;
        card.verify()?;
        identity = card.read()?;
        if !identity_ok(&identity, Some(&request.iccid)) {
            return Err(switched.err().unwrap_or_else(|| {
                Failure::new("profile_switch_unconfirmed", "切卡后未读到目标 Profile")
            }));
        }
    }
    let facts = card.verify()?;
    card.notifications(&facts)?;
    let facts = card.verify()?;
    identity = card.read()?;
    if !identity_ok(&identity, Some(&request.iccid)) {
        return Err(Failure::new(
            "multistandby_identity_changed",
            "Profile 物理身份验证失败；未执行 AKA",
        ));
    }
    card.fresh()?;
    if request.action == Action::Authenticate
        && identity["imsi"].as_str() != Some(request.expected_imsi.as_str())
    {
        return Err(Failure::new(
            "multistandby_identity_changed",
            "Profile IMSI 已变化，需重建会话",
        ));
    }
    card.publish(&identity, facts)?;
    let aka = if request.action == Action::Authenticate {
        Some(card.authenticate()?)
    } else {
        None
    };
    Ok(json!({"identity":identity,"aka":aka}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Script {
        log: Vec<&'static str>,
        reads: VecDeque<Result<Value, Failure>>,
        verifies: VecDeque<Result<(), Failure>>,
        enable_error: bool,
        notification_error: bool,
        cancel_on_enable: bool,
        cancelled: bool,
    }
    impl Script {
        fn new(reads: &[(&str, &str)]) -> Self {
            Self {
                log: vec![],
                reads: reads
                    .iter()
                    .map(|(iccid, imsi)| Ok(json!({"iccid":iccid,"imsi":imsi})))
                    .collect(),
                verifies: VecDeque::new(),
                enable_error: false,
                notification_error: false,
                cancel_on_enable: false,
                cancelled: false,
            }
        }
    }
    impl Card for Script {
        fn fresh(&mut self) -> Result<(), Failure> {
            self.log.push("fresh");
            if self.cancelled {
                Err(Failure::new("cancelled", "cancelled"))
            } else {
                Ok(())
            }
        }
        fn verify(&mut self) -> Result<crate::reader_esim_download::EuiccFacts, Failure> {
            self.log.push("eid");
            self.verifies.pop_front().unwrap_or(Ok(()))?;
            Ok(crate::reader_esim_download::EuiccFacts::default())
        }
        fn read(&mut self) -> Result<Value, Failure> {
            self.log.push("identity");
            self.reads.pop_front().expect("unexpected identity read")
        }
        fn reset(&mut self, power: bool) -> Result<(), Failure> {
            self.log
                .push(if power { "recover_reset" } else { "reopen" });
            Ok(())
        }
        fn enable(&mut self) -> Result<(), Failure> {
            self.log.push("enable");
            if self.cancel_on_enable {
                self.cancelled = true;
            }
            if self.enable_error {
                Err(Failure::new("enable_unknown", "response lost"))
            } else {
                Ok(())
            }
        }
        fn recovery(&mut self) -> Result<(), Failure> {
            self.log.push("recovery");
            Ok(())
        }
        fn notifications(
            &mut self,
            _: &crate::reader_esim_download::EuiccFacts,
        ) -> Result<(), Failure> {
            self.log.push("notifications");
            if self.notification_error {
                Err(Failure::new("notification_pending", "not delivered"))
            } else {
                Ok(())
            }
        }
        fn authenticate(&mut self) -> Result<Value, Failure> {
            self.log.push("aka");
            Ok(json!({"outcome":"success"}))
        }
        fn publish(
            &mut self,
            _: &Value,
            _: crate::reader_esim_download::EuiccFacts,
        ) -> Result<(), Failure> {
            self.log.push("publish");
            Ok(())
        }
    }
    fn request(action: &str) -> Submit {
        serde_json::from_value(json!({"operation_id":"workflow-test","action":action,"reader":"Test Reader","reader_generation":7,"transaction":{"kind":"operation","binding":{"producer_id":"test-producer","operation_token":"test-operation","operation_sequence":1,"reader":"Test Reader","reader_generation":7,"purpose":"card","authority":{"kind":"idle"}}},"eid":"89049032000000000000000000000001","iccid":"8901000000000000001","expected_imsi":"001010000000001","application":"isim","rand":vec![1;16],"autn":vec![2;16]})).unwrap()
    }
    const TARGET: &str = "8901000000000000001";
    const IMSI: &str = "001010000000001";
    #[test]
    fn multistandby_identity_reads_live_between_two_eid_checks() {
        let mut card = Script::new(&[(TARGET, IMSI)]);
        let result = run(&mut card, &request("identity")).unwrap();
        assert_eq!(result["identity"]["iccid"], TARGET);
        assert_eq!(card.log, vec!["fresh", "eid", "identity", "eid", "publish"]);
    }
    #[test]
    fn multistandby_same_profile_still_synchronizes_notifications_before_aka() {
        let mut card = Script::new(&[(TARGET, IMSI), (TARGET, IMSI)]);
        run(&mut card, &request("authenticate")).unwrap();
        assert_eq!(
            card.log,
            vec![
                "fresh",
                "eid",
                "identity",
                "eid",
                "notifications",
                "eid",
                "identity",
                "fresh",
                "publish",
                "aka"
            ]
        );
    }
    #[test]
    fn multistandby_lost_enable_response_recovers_without_replay_and_then_authenticates() {
        let mut card = Script::new(&[
            ("8901000000000000002", IMSI),
            (TARGET, IMSI),
            (TARGET, IMSI),
        ]);
        card.enable_error = true;
        run(&mut card, &request("authenticate")).unwrap();
        assert_eq!(card.log.iter().filter(|s| **s == "enable").count(), 1);
        assert_eq!(
            &card.log[3..8],
            &["enable", "recovery", "recover_reset", "eid", "identity"]
        );
        assert_eq!(card.log.last(), Some(&"aka"));
    }
    #[test]
    fn multistandby_cancelled_enable_retains_recovery_but_never_authenticates_or_publishes() {
        let mut card = Script::new(&[
            ("8901000000000000002", IMSI),
            (TARGET, IMSI),
            (TARGET, IMSI),
        ]);
        card.cancel_on_enable = true;
        assert_eq!(
            run(&mut card, &request("authenticate")).unwrap_err().code,
            "cancelled"
        );
        assert!(card.log.contains(&"recover_reset"));
        assert!(card.log.contains(&"notifications"));
        assert!(!card.log.contains(&"aka"));
        assert!(!card.log.contains(&"publish"));
    }
    #[test]
    fn multistandby_eid_mismatch_never_reopens_switches_or_authenticates() {
        let mut card = Script::new(&[]);
        card.verifies.push_back(Err(Failure::new(
            "multistandby_identity_changed",
            "swapped card",
        )));
        assert_eq!(
            run(&mut card, &request("authenticate")).unwrap_err().code,
            "multistandby_identity_changed"
        );
        assert_eq!(card.log, vec!["fresh", "eid"]);
    }
    #[test]
    fn multistandby_transient_eid_error_reopens_then_revalidates() {
        let mut card = Script::new(&[(TARGET, IMSI), (TARGET, IMSI)]);
        card.verifies
            .push_back(Err(Failure::new("reader_metadata_failed", "reset handle")));
        run(&mut card, &request("prepare")).unwrap();
        assert_eq!(
            &card.log[..6],
            &["fresh", "eid", "fresh", "reopen", "eid", "identity"]
        );
        assert!(!card.log.contains(&"aka"));
    }
    #[test]
    fn multistandby_imsi_change_and_pending_notification_both_deny_aka() {
        let mut card = Script::new(&[(TARGET, IMSI), (TARGET, "001010000000002")]);
        assert_eq!(
            run(&mut card, &request("authenticate")).unwrap_err().code,
            "multistandby_identity_changed"
        );
        assert!(!card.log.contains(&"aka"));
        let mut card = Script::new(&[(TARGET, IMSI)]);
        card.notification_error = true;
        assert_eq!(
            run(&mut card, &request("authenticate")).unwrap_err().code,
            "notification_pending"
        );
        assert!(!card.log.contains(&"aka"));
    }
    #[test]
    fn multistandby_unconfirmed_switch_preserves_unknown_error_and_never_replays() {
        let mut card = Script::new(&[("8901000000000000002", IMSI), ("8901000000000000002", IMSI)]);
        card.enable_error = true;
        assert_eq!(
            run(&mut card, &request("authenticate")).unwrap_err().code,
            "enable_unknown"
        );
        assert_eq!(card.log.iter().filter(|s| **s == "enable").count(), 1);
        assert!(!card.log.contains(&"aka"));
    }
}
