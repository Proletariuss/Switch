//! An owned certificate request crosses the platform trust boundary. A parsed
//! CERT, encrypted SK packet, or EAP success alone never satisfies this policy.
use proletarius_ike::{
    PayloadFrame,
    cert_auth::{
        CertificateAuthError, SignatureAlgorithm, certificate_chain, certificate_signature,
    },
};
use std::{future::Future, pin::Pin, sync::Arc};
use zeroize::Zeroizing;

pub type CertificateVerifyFuture =
    Pin<Box<dyn Future<Output = Result<(), CertificateAuthError>> + Send>>;
pub trait PeerCertificateVerifier: Send + Sync {
    fn verify(&self, request: CertificateAuthRequest) -> CertificateVerifyFuture;
}
pub struct CertificateAuthRequest {
    pub certificates: Vec<Vec<u8>>,
    pub expected_name: String,
    pub algorithm: SignatureAlgorithm,
    pub signature: Vec<u8>,
    pub signed_octets: Zeroizing<Vec<u8>>,
}
#[derive(Clone)]
pub struct PeerCertificatePolicy {
    required: bool,
    expected_name: Arc<str>,
    verifier: Arc<dyn PeerCertificateVerifier>,
}
impl PeerCertificatePolicy {
    pub fn new(
        expected_name: &str,
        verifier: Arc<dyn PeerCertificateVerifier>,
    ) -> Result<Self, CertificateAuthError> {
        if !valid_peer_name(expected_name) {
            return Err(CertificateAuthError::MissingIdentity);
        }
        Ok(Self {
            required: true,
            expected_name: expected_name.into(),
            verifier,
        })
    }
    pub fn optional(
        expected_name: &str,
        verifier: Arc<dyn PeerCertificateVerifier>,
    ) -> Result<Self, CertificateAuthError> {
        let mut policy = Self::new(expected_name, verifier)?;
        policy.required = false;
        Ok(policy)
    }
    pub fn requires_certificate(&self) -> bool {
        self.required
    }
    pub fn expected_name(&self) -> &str {
        &self.expected_name
    }
}
/// No DNS lookup: the caller freezes the selected ePDG or explicit certificate
/// hostname. IP literals remain IP literals in the platform IPsec policy.
pub fn valid_peer_name(name: &str) -> bool {
    if name.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !name.is_empty()
        && name.len() <= 253
        && name.is_ascii()
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
impl crate::IkeAuthSession {
    /// Freeze the trust owner before the first encrypted IKE_AUTH exchange.
    pub fn with_peer_certificate_policy(mut self, policy: PeerCertificatePolicy) -> Self {
        self.peer_certificate_policy = Some(policy);
        self.initial_certificate_verified = false;
        self
    }
    pub(crate) fn set_peer_certificate_policy(&mut self, policy: Option<PeerCertificatePolicy>) {
        self.peer_certificate_policy = policy;
        self.initial_certificate_verified = false;
    }
    pub(crate) async fn verify_initial_certificate(
        &mut self,
        frames: &[PayloadFrame],
        first: bool,
    ) -> Result<(), CertificateAuthError> {
        let has_certificate = frames.iter().any(|f| matches!(f.kind, 37 | 39));
        if !first {
            if has_certificate {
                return Err(CertificateAuthError::DuplicateAuthentication);
            }
            return Ok(());
        }
        let Some(policy) = self.peer_certificate_policy.as_ref() else {
            // EAP-only is explicitly supported, but an unsolicited signature
            // must never be silently accepted without its trust owner.
            return if has_certificate {
                Err(CertificateAuthError::VerificationUnavailable)
            } else {
                Ok(())
            };
        };
        if !policy.required && !has_certificate {
            return Ok(());
        }
        let mut identities = frames.iter().filter(|f| f.kind == 36);
        let id = identities
            .next()
            .ok_or(CertificateAuthError::MissingIdentity)?;
        if identities.next().is_some()
            || id.body.len() < 5
            || !matches!(id.body[0], 1 | 2 | 3 | 5 | 9 | 10 | 11)
        {
            return Err(CertificateAuthError::MissingIdentity);
        }
        let chain = certificate_chain(frames)?;
        let signature =
            certificate_signature(frames, &self.sa_init_request, &self.sa_init_response)?;
        let signed_octets = proletarius_ike::auth_signed_octets(
            self.prf_id,
            &self.sa_init_response,
            &self.initiator_nonce,
            &self.keys.sk_pr,
            &id.body,
        )
        .map_err(|_| CertificateAuthError::InvalidTranscript)?;
        let request = CertificateAuthRequest {
            certificates: chain.iter().map(|v| v.to_vec()).collect(),
            expected_name: policy.expected_name.to_string(),
            algorithm: signature.algorithm,
            signature: signature.signature.to_vec(),
            signed_octets,
        };
        policy.verifier.verify(request).await?;
        self.initial_certificate_verified = true;
        Ok(())
    }
}
