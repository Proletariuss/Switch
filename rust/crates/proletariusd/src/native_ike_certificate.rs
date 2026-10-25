//! System-trusted IKE responder authentication. No user CA, network fetch,
//! hostname resolution, or certificate-validation bypass is exposed.
use proletarius_ike::cert_auth::CertificateAuthError;
use proletarius_vowifi::{
    CertificateAuthRequest, CertificateVerifyFuture, PeerCertificatePolicy,
    PeerCertificateVerifier, carrier::NativeCarrierTunnelConfig,
};
use std::sync::Arc;

pub(crate) fn policy(
    config: &NativeCarrierTunnelConfig,
) -> Result<Option<PeerCertificatePolicy>, CertificateAuthError> {
    #[cfg(not(target_os = "macos"))]
    return if config.validate_remote_certificate {
        Err(CertificateAuthError::VerificationUnavailable)
    } else {
        Ok(None)
    };
    #[cfg(target_os = "macos")]
    {
        let verifier = Arc::new(SystemVerifier);
        if config.validate_remote_certificate {
            PeerCertificatePolicy::new(config.certificate_peer_name(), verifier).map(Some)
        } else {
            PeerCertificatePolicy::optional(config.certificate_peer_name(), verifier).map(Some)
        }
    }
}
struct SystemVerifier;
impl PeerCertificateVerifier for SystemVerifier {
    fn verify(&self, request: CertificateAuthRequest) -> CertificateVerifyFuture {
        static WORKERS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let workers = WORKERS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone();
        Box::pin(run_bounded(request, workers, evaluate))
    }
}
async fn run_bounded(
    request: CertificateAuthRequest,
    workers: Arc<tokio::sync::Semaphore>,
    verify: impl FnOnce(CertificateAuthRequest) -> Result<(), CertificateAuthError> + Send + 'static,
) -> Result<(), CertificateAuthError> {
    let permit = workers
        .acquire_owned()
        .await
        .map_err(|_| CertificateAuthError::VerificationUnavailable)?;
    tokio::task::spawn_blocking(move || {
        let _actual_worker = permit;
        verify(request)
    })
    .await
    .map_err(|_| CertificateAuthError::VerificationUnavailable)?
}
#[cfg(not(target_os = "macos"))]
fn evaluate(_: CertificateAuthRequest) -> Result<(), CertificateAuthError> {
    Err(CertificateAuthError::VerificationUnavailable)
}
#[cfg(target_os = "macos")]
fn evaluate(request: CertificateAuthRequest) -> Result<(), CertificateAuthError> {
    platform::evaluate(request, None)
}
#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use proletarius_ike::cert_auth::{
        MAX_CERTIFICATE_BYTES, MAX_CERTIFICATES, MAX_CHAIN_BYTES, SignatureAlgorithm,
    };
    use security_framework::{certificate::SecCertificate, key::Algorithm, trust::SecTrust};
    pub(super) fn evaluate(
        request: CertificateAuthRequest,
        anchors: Option<&[SecCertificate]>,
    ) -> Result<(), CertificateAuthError> {
        if request.certificates.is_empty() {
            return Err(CertificateAuthError::MissingCertificate);
        }
        if request.certificates.len() > MAX_CERTIFICATES
            || request
                .certificates
                .iter()
                .any(|v| v.len() > MAX_CERTIFICATE_BYTES)
            || request.certificates.iter().map(Vec::len).sum::<usize>() > MAX_CHAIN_BYTES
        {
            return Err(CertificateAuthError::ChainTooLarge);
        }
        let certs: Vec<_> = request
            .certificates
            .iter()
            .map(|der| {
                SecCertificate::from_der(der).map_err(|_| CertificateAuthError::InvalidCertificate)
            })
            .collect::<Result<_, _>>()?;
        let policy = proletarius_ike_trust::ipsec_policy(&request.expected_name)
            .map_err(|_| CertificateAuthError::VerificationUnavailable)?;
        let mut trust = SecTrust::create_with_certificates(&certs, &[policy])
            .map_err(|_| CertificateAuthError::VerificationUnavailable)?;
        trust
            .set_network_fetch_allowed(false)
            .map_err(|_| CertificateAuthError::VerificationUnavailable)?;
        // This argument is never supplied by the production entry point.
        // Explicit anchors exist only inside the unit-test trust boundary.
        if let Some(anchors) = anchors {
            #[cfg(not(test))]
            {
                let _ = anchors;
                return Err(CertificateAuthError::VerificationUnavailable);
            }
            #[cfg(test)]
            {
                trust
                    .set_anchor_certificates(anchors)
                    .map_err(|_| CertificateAuthError::VerificationUnavailable)?;
                trust
                    .set_trust_anchor_certificates_only(true)
                    .map_err(|_| CertificateAuthError::VerificationUnavailable)?;
            }
        }
        trust.evaluate_with_error().map_err(|error| {
            #[cfg(test)]
            eprintln!("test trust error: {error:?}");
            #[cfg(not(test))]
            let _ = error;
            CertificateAuthError::UntrustedCertificate
        })?;
        let key = proletarius_ike_trust::copy_trust_key(&trust)
            .map_err(|_| CertificateAuthError::InvalidCertificate)?;
        let algorithm = match request.algorithm {
            SignatureAlgorithm::RsaPkcs1Sha1 => Algorithm::RSASignatureMessagePKCS1v15SHA1,
            SignatureAlgorithm::RsaPkcs1Sha256 => Algorithm::RSASignatureMessagePKCS1v15SHA256,
            SignatureAlgorithm::RsaPkcs1Sha384 => Algorithm::RSASignatureMessagePKCS1v15SHA384,
            SignatureAlgorithm::RsaPkcs1Sha512 => Algorithm::RSASignatureMessagePKCS1v15SHA512,
            SignatureAlgorithm::EcdsaSha256 => Algorithm::ECDSASignatureMessageX962SHA256,
            SignatureAlgorithm::EcdsaSha384 => Algorithm::ECDSASignatureMessageX962SHA384,
        };
        if !key
            .verify_signature(algorithm, &request.signed_octets, &request.signature)
            .map_err(|_| CertificateAuthError::InvalidSignature)?
        {
            return Err(CertificateAuthError::InvalidSignature);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
