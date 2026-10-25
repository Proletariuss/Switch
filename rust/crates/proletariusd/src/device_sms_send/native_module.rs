//! Module IMS uses the same durable claim and real native SIP batch owner as
//! ordinary reader IMS, with an additional physical profile/RF validation.
use super::*;
use crate::native_vowifi_session::{NativeLifecycle, NativeSmsBinding};

pub(super) struct RuntimeModuleSms(pub(super) Arc<NativeLifecycle>);
impl ReaderSms for RuntimeModuleSms {
    fn has_intent(&self) -> bool {
        self.0
            .retained_module_service()
            .is_some_and(|service| service.owns_session())
    }
    fn capture(&self) -> std::result::Result<NativeSmsBinding, &'static str> {
        self.0
            .retained_module_service()
            .ok_or("native_module_sms_unavailable")?
            .sms_binding()
    }
    fn resolve(&self, token: &str) -> std::result::Result<NativeSmsBinding, &'static str> {
        self.0
            .retained_module_service()
            .ok_or("native_module_sms_unavailable")?
            .resolve_sms_binding(token)
    }
    fn validate_business(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<(), &'static str>> + Send + '_>,
    > {
        Box::pin(async {
            self.0
                .retained_module_service()
                .ok_or("native_module_sms_unavailable")?
                .validate_business()
                .await
        })
    }
}
impl Service {
    pub(super) fn capture_native_module(&self, body: &SendRequest) -> Result<Binding> {
        let route = self
            .native_module_sms
            .capture()
            .map_err(|_| native_reader::expired())?;
        if !body.expected_subscription_id.is_empty()
            && body.expected_subscription_id != route.subscription_id
        {
            return Err(conflict());
        }
        let binding = format!("{RUST_MODULE_IMS_BINDING_PREFIX}{}", route.binding);
        if !valid_token(&binding)
            || route.subscription_id.is_empty()
            || !valid_token(&route.subscription_id)
        {
            return Err(unavailable());
        }
        Ok(Binding {
            binding,
            device: "dji".into(),
            subscription_id: route.subscription_id,
            owner: "rust_module_ims".into(),
            expected_iccid: String::new(),
        })
    }
}
