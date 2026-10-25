//! Typed in-process entry into the same retained physical workflow used by the
//! public adapter. Completion still means the actual worker has joined.
use super::*;
use crate::reader_session_lease::{LeaseIdentity, ReaderAkaApplication};
use proletarius_vowifi::SimAkaResult;

pub(crate) enum NativeAction {
    Identity,
    Prepare,
    Authenticate {
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    },
}
pub(crate) struct NativeRequest {
    pub(crate) transaction: CardTransaction,
    pub(crate) pcsc: Option<Binding>,
    pub(crate) eid: String,
    pub(crate) aid: String,
    pub(crate) iccid: String,
    pub(crate) expected_imsi: String,
    pub(crate) action: NativeAction,
}
pub(crate) struct NativeResult {
    pub(crate) identity: LeaseIdentity,
    pub(crate) aka: Option<SimAkaResult>,
}
impl Service {
    pub(crate) async fn inventory_native(
        &self,
        transaction: CardTransaction,
        pcsc: Option<Binding>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Value, Failure> {
        let CardTransaction::Operation { binding } = &transaction else {
            return Err(Failure::invalid());
        };
        let request = Submit {
            operation_id: crate::multistandby_store::random_token()
                .map_err(|_| Failure::new("entropy_unavailable", "任务标识生成失败"))?,
            action: Action::Inventory,
            reader: binding.reader.clone(),
            reader_generation: binding.reader_generation,
            transaction,
            pcsc,
            eid: String::new(),
            aid: String::new(),
            iccid: String::new(),
            expected_imsi: String::new(),
            application: String::new(),
            rand: None,
            autn: None,
        };
        let (reply, wait) = tokio::sync::oneshot::channel();
        self.submit_cancelled(request, Some(cancelled), Some(reply))?;
        wait.await
            .map_err(|_| Failure::new("multistandby_worker_failed", "卡片工作线程异常结束"))?
    }
    pub(crate) async fn run_native(
        &self,
        input: NativeRequest,
        cancelled: Arc<AtomicBool>,
    ) -> Result<NativeResult, Failure> {
        let CardTransaction::Operation { binding } = &input.transaction else {
            return Err(Failure::invalid());
        };
        let (action, application, rand, autn) = match input.action {
            NativeAction::Identity => (Action::Identity, String::new(), None, None),
            NativeAction::Prepare => (Action::Prepare, String::new(), None, None),
            NativeAction::Authenticate {
                application,
                rand,
                autn,
            } => (
                Action::Authenticate,
                match application {
                    ReaderAkaApplication::Usim => "usim",
                    ReaderAkaApplication::Isim => "isim",
                }
                .into(),
                Some(rand),
                Some(autn),
            ),
        };
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy)
            .map_err(|_| Failure::new("entropy_unavailable", "任务标识生成失败"))?;
        let id = format!(
            "native-multi-{}",
            crate::reader_esim_download::encode_hex_lower(&entropy)
        );
        let request = Submit {
            operation_id: id.clone(),
            action,
            reader: binding.reader.clone(),
            reader_generation: binding.reader_generation,
            transaction: input.transaction,
            pcsc: input.pcsc,
            eid: input.eid,
            aid: input.aid,
            iccid: input.iccid,
            expected_imsi: input.expected_imsi,
            application,
            rand,
            autn,
        };
        let (reply, wait) = tokio::sync::oneshot::channel();
        self.submit_cancelled(request, Some(cancelled), Some(reply))?;
        // A native waiter owns its result independently of bounded HTTP receipt
        // eviction. Only the retained worker can send this terminal value.
        let result = wait
            .await
            .map_err(|_| Failure::new("multistandby_worker_failed", "卡片工作线程异常结束"))??;
        let identity = LeaseIdentity::from_metadata(result["identity"].clone())
            .map_err(|e| Failure::new("reader_metadata_failed", e))?;
        let aka = &result["aka"];
        let decode = || -> Option<SimAkaResult> {
            Some(match aka["outcome"].as_str()? {
                "success" => SimAkaResult::Success {
                    res: serde_json::from_value(aka["res"].clone()).ok()?,
                    ck: serde_json::from_value(aka["ck"].clone()).ok()?,
                    ik: serde_json::from_value(aka["ik"].clone()).ok()?,
                },
                "synchronization_failure" => SimAkaResult::SynchronizationFailure {
                    auts: serde_json::from_value(aka["auts"].clone()).ok()?,
                },
                "authentication_rejected" => SimAkaResult::AuthenticationRejected,
                _ => return None,
            })
        };
        let aka = if action == Action::Authenticate {
            Some(decode().ok_or_else(|| {
                Failure::new("multistandby_authentication_failed", "AKA 结果无效")
            })?)
        } else {
            None
        };
        Ok(NativeResult { identity, aka })
    }
}
