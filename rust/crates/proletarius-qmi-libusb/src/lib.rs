//! Switch-owned USB device selection shared by platform transport adapters.
//!
//! The macOS module opens one candidate at a time, claims its QMI interface,
//! and validates it with a real CTL_SYNC before exposing the client.

pub mod selection;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{QmiUsbClient, QmiUsbInfo, open_qmi_client};

/// A probe failed after acquiring a client whose final cleanup was not
/// confirmed. Callers must retain this receipt and must not probe another
/// interface as though no owner had been acquired.
#[derive(Debug)]
pub struct ProbeCleanupFailure {
    pub probe_failure: String,
    pub cleanup_failure: String,
}

impl std::fmt::Display for ProbeCleanupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}; QMI probe cleanup unconfirmed: {}",
            self.probe_failure, self.cleanup_failure
        )
    }
}

impl std::error::Error for ProbeCleanupFailure {}

#[cfg(any(target_os = "macos", test))]
fn confirm_failed_probe_cleanup(
    probe_failure: String,
    cleanup: Result<(), proletarius_qmi_core::ClientError>,
) -> std::io::Result<()> {
    cleanup.map_err(|error| {
        std::io::Error::other(ProbeCleanupFailure {
            probe_failure,
            cleanup_failure: error.to_string(),
        })
    })
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    #[test]
    fn confirmed_probe_cleanup_allows_next_candidate() {
        assert!(confirm_failed_probe_cleanup("CTL_SYNC failed".into(), Ok(())).is_ok());
    }

    #[test]
    fn unknown_probe_cleanup_is_typed_and_stops_candidate_scan() {
        let mut candidates = 0;
        let result = (|| -> std::io::Result<()> {
            candidates += 1;
            confirm_failed_probe_cleanup(
                "CTL_SYNC failed on interface 3".into(),
                Err(proletarius_qmi_core::ClientError::Io(
                    std::io::Error::other("reader join failed"),
                )),
            )?;
            candidates += 1;
            Ok(())
        })();
        let error = result.unwrap_err();
        let receipt = error
            .get_ref()
            .unwrap()
            .downcast_ref::<ProbeCleanupFailure>()
            .unwrap();
        assert_eq!(receipt.probe_failure, "CTL_SYNC failed on interface 3");
        assert!(receipt.cleanup_failure.contains("reader join failed"));
        assert_eq!(candidates, 1);
    }
}
