//! The narrow macOS IPsec policy/key ABI that security-framework does not wrap.
//! Certificate evaluation and signature verification remain in the safe owner.
#![deny(unsafe_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyError {
    InvalidName,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    Unavailable,
}

#[cfg(target_os = "macos")]
pub use platform::{copy_trust_key, ipsec_policy};
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod platform {
    use super::{KeyError, PolicyError};
    use core_foundation::{
        base::{CFTypeRef, TCFType},
        dictionary::{CFDictionary, CFDictionaryRef},
        string::{CFString, CFStringRef},
    };
    use security_framework::{key::SecKey, policy::SecPolicy, trust::SecTrust};
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecTrustCopyKey(trust: <SecTrust as TCFType>::Ref) -> <SecKey as TCFType>::Ref;
        static kSecPolicyAppleIPsec: CFStringRef;
        static kSecPolicyName: CFStringRef;
        fn SecPolicyCreateWithProperties(
            identifier: CFTypeRef,
            properties: CFDictionaryRef,
        ) -> <SecPolicy as TCFType>::Ref;
    }
    /// Extract a leaf key using the macOS 11+ API. This does not grant trust:
    /// the caller must first complete its platform trust evaluation, and must
    /// subsequently verify the actual IKE transcript signature with this key.
    pub fn copy_trust_key(trust: &SecTrust) -> Result<SecKey, KeyError> {
        // The borrowed trust remains alive throughout the synchronous call.
        // Apple's API is nullable even after a successful trust evaluation.
        let raw = unsafe { SecTrustCopyKey(trust.as_concrete_TypeRef()) };
        if raw.is_null() {
            return Err(KeyError::Unavailable);
        }
        // Copy rule transfers the returned +1 reference into RAII once.
        Ok(unsafe { SecKey::wrap_under_create_rule(raw) })
    }

    pub fn ipsec_policy(name: &str) -> Result<SecPolicy, PolicyError> {
        let valid = name.parse::<std::net::IpAddr>().is_ok()
            || (!name.is_empty()
                && name.len() <= 253
                && name.is_ascii()
                && name.split('.').all(|v| {
                    !v.is_empty()
                        && v.len() <= 63
                        && !v.starts_with('-')
                        && !v.ends_with('-')
                        && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }));
        if !valid {
            return Err(PolicyError::InvalidName);
        }
        let hostname = CFString::new(name);
        // Public Security constants are immutable, process-lifetime CFStrings;
        // Get rule retains the key while the dictionary owns its references.
        let key = unsafe { CFString::wrap_under_get_rule(kSecPolicyName) };
        let properties = CFDictionary::from_CFType_pairs(&[(key, hostname)]);
        // Both references stay alive through this synchronous system call.
        let raw = unsafe {
            SecPolicyCreateWithProperties(
                kSecPolicyAppleIPsec.cast(),
                properties.as_concrete_TypeRef(),
            )
        };
        if raw.is_null() {
            return Err(PolicyError::Unavailable);
        }
        // Create rule transfers the +1 reference into RAII exactly once.
        Ok(unsafe { SecPolicy::wrap_under_create_rule(raw) })
    }
}
#[cfg(not(target_os = "macos"))]
pub fn ipsec_policy(_: &str) -> Result<(), PolicyError> {
    Err(PolicyError::Unavailable)
}
