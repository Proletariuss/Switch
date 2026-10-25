//! Full module identity over the actor-owned retained serial basic channel.
//! No metadata cache or JSON request can manufacture this capability.
use crate::modem_actor::{ModemActor, SerialModuleHold};
use crate::module_sim_identity;
use crate::reader_session_lease::ReaderAkaApplication;
use crate::usb_at::Failure;
use hyper::StatusCode;
use proletarius_vowifi::SimAkaResult;
use serde_json::Value;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

#[derive(Clone)]
pub(crate) struct SerialModuleIdentity {
    actor: ModemActor,
    hold: SerialModuleHold,
    metadata: Value,
    iccid: Zeroizing<String>,
    imsi: Zeroizing<String>,
}
fn changed() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "module_sim_identity_changed",
        "模块 SIM 身份已变更",
    )
}
impl SerialModuleIdentity {
    pub(crate) async fn capture(
        actor: &ModemActor,
        hold: &SerialModuleHold,
    ) -> Result<Self, Failure> {
        let identity = actor
            .sim_identity_transaction(hold, move |command| {
                module_sim_identity::read_identity(
                    command,
                    Instant::now() + Duration::from_secs(60),
                )
            })
            .await?;
        if !actor.serial_module_current(hold) {
            return Err(changed());
        }
        let mut metadata = identity.metadata;
        if metadata["phone_number"]
            .as_str()
            .is_none_or(|v| v.trim().is_empty())
        {
            metadata["phone_number"] = Value::String(identity.phone_number);
        }
        Ok(Self {
            actor: actor.clone(),
            hold: hold.clone(),
            metadata,
            iccid: identity.iccid,
            imsi: identity.imsi,
        })
    }
    pub(crate) fn metadata(&self) -> Value {
        self.metadata.clone()
    }
    pub(crate) fn generation(&self) -> u64 {
        self.hold.binding().sim_generation
    }
    pub(crate) fn current(&self) -> bool {
        self.actor.serial_module_current(&self.hold)
    }
    pub(crate) async fn validate(&self) -> Result<(), Failure> {
        if !self.current() {
            return Err(changed());
        }
        let iccid = self.iccid.clone();
        let imsi = self.imsi.clone();
        let response = self
            .actor
            .sim_identity_transaction(&self.hold, move |command| {
                module_sim_identity::profile_check(
                    command,
                    &iccid,
                    &imsi,
                    Instant::now() + Duration::from_secs(12),
                )
            })
            .await?;
        if !self.current() {
            return Err(changed());
        }
        match response["outcome"].as_str() {
            Some("identity_matches") => Ok(()),
            Some("identity_changed") => Err(changed()),
            _ => Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_sim_identity_failed",
                "模块 SIM 身份读取失败",
            )),
        }
    }
    pub(crate) async fn authenticate(
        &self,
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> Result<SimAkaResult, Failure> {
        let rand = Zeroizing::new(rand);
        let autn = Zeroizing::new(autn);
        if !self.current() {
            return Err(changed());
        }
        let iccid = self.iccid.clone();
        let imsi = self.imsi.clone();
        let result = self
            .actor
            .sim_identity_transaction(&self.hold, move |command| {
                module_sim_identity::authenticate(command, application, &iccid, &imsi, &rand, &autn)
            })
            .await?;
        if !self.current() {
            return Err(changed());
        }
        Ok(result)
    }
}
