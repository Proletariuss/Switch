// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Bounded, synchronous GSMA SGP.22 consumer LPA.
//!
//! Card I/O and HTTPS are explicit caller-owned dependencies.  The state
//! machine creates no thread or async runtime and only checks cancellation at
//! protocol boundaries; an in-flight HTTPS/APDU operation is bounded by its
//! transport timeout.

mod activation;
mod es9;
mod es9_http;
mod profile_metadata;

use std::{
    fmt,
    time::{Duration, Instant},
};

use proletarius_euicc::{
    CancelSessionReason, CardTransport, Euicc, EuiccInfoVersion, InstalledProfile,
    PendingNotification, PrepareDownloadRequest, ServerAuthenticationRequest,
};
use sha2::{Digest as _, Sha256};

pub use activation::{ActivationCode, ActivationCodeError};
pub use es9::{
    AuthenticateResponse, BoundProfilePackage, Es9Transport, HttpError, InitiateResponse,
};
pub use es9_http::HttpEs9Client;
pub use profile_metadata::{
    ProfileMetadata, ProfileMetadataError, estimate_profile_size, parse_profile_metadata,
};

pub const MAX_DOWNLOAD_DURATION: Duration = Duration::from_secs(300);
/// Notifications are best-effort and must not hold an eUICC operation open
/// for the full profile-download timeout when an SM-DP+ is unreachable.
pub const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(15);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadStage {
    InitiateAuthentication,
    AuthenticateEuicc,
    AuthenticateClient,
    AuthenticateServer,
    Install,
    Done,
}

pub trait DownloadObserver {
    fn on_stage(&mut self, _stage: DownloadStage) {}

    fn is_cancelled(&self) -> bool {
        false
    }

    fn confirm_profile(&mut self, _profile_metadata: &[u8]) -> bool {
        true
    }

    fn confirm_profile_details(
        &mut self,
        encoded_metadata: &[u8],
        _metadata: &ProfileMetadata,
        _estimated_profile_size_bytes: i64,
    ) -> bool {
        self.confirm_profile(encoded_metadata)
    }

    fn on_warning(&mut self, _message: &str) {}
}

#[derive(Default)]
pub struct NoopObserver;

impl DownloadObserver for NoopObserver {}

pub trait RspCard {
    type Error: fmt::Display;

    fn challenge(&mut self) -> Result<Vec<u8>, Self::Error>;
    fn euicc_info1(&mut self) -> Result<Vec<u8>, Self::Error>;
    fn authenticate_server(
        &mut self,
        input: ServerAuthenticationRequest<'_>,
    ) -> Result<Vec<u8>, Self::Error>;
    /// Submit the fixed pre-AuthenticateServer B0 authentication body.
    /// Implementations for cards without this vendor command may reject it;
    /// the normal LPA flow never calls it unless the caller's auth hook does.
    fn transmit_b0_authentication(&mut self, body: &[u8]) -> Result<Vec<u8>, Self::Error>;
    fn prepare_download(
        &mut self,
        input: PrepareDownloadRequest<'_>,
    ) -> Result<Vec<u8>, Self::Error>;
    fn install(&mut self, package: &[u8]) -> Result<InstalledProfile, Self::Error>;
    fn cancel(
        &mut self,
        transaction_id: &[u8],
        reason: CancelSessionReason,
    ) -> Result<Vec<u8>, Self::Error>;
    fn retrieve_notification(
        &mut self,
        sequence_number: u64,
    ) -> Result<Option<PendingNotification>, Self::Error>;
    fn remove_notification(&mut self, sequence_number: u64) -> Result<(), Self::Error>;
}

impl<T> RspCard for Euicc<T>
where
    T: CardTransport,
    T::Error: fmt::Display,
{
    type Error = proletarius_euicc::Error<T::Error>;

    fn challenge(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.get_euicc_challenge()
    }

    fn euicc_info1(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.get_euicc_info_raw(EuiccInfoVersion::V1)
    }

    fn authenticate_server(
        &mut self,
        input: ServerAuthenticationRequest<'_>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.authenticate_server(input)
    }

    fn transmit_b0_authentication(&mut self, body: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.transmit_b0_authentication(body)
    }

    fn prepare_download(
        &mut self,
        input: PrepareDownloadRequest<'_>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.prepare_download(input)
    }

    fn install(&mut self, package: &[u8]) -> Result<InstalledProfile, Self::Error> {
        self.install_bound_profile_package(package)
    }

    fn cancel(
        &mut self,
        transaction_id: &[u8],
        reason: CancelSessionReason,
    ) -> Result<Vec<u8>, Self::Error> {
        self.cancel_rsp_session(transaction_id, reason)
    }

    fn retrieve_notification(
        &mut self,
        sequence_number: u64,
    ) -> Result<Option<PendingNotification>, Self::Error> {
        self.retrieve_notification(sequence_number)
    }

    fn remove_notification(&mut self, sequence_number: u64) -> Result<(), Self::Error> {
        self.remove_notification(sequence_number)
    }
}

#[derive(Debug)]
pub enum Error<CardError, NetworkError> {
    Card(CardError),
    Network(NetworkError),
    Authentication(String),
    Invalid(&'static str),
    ConfirmationCodeRequired,
    Rejected,
    Cancelled,
    TimedOut,
    CancelFailed {
        original: &'static str,
        detail: String,
    },
}

impl<C: fmt::Display, N: fmt::Display> fmt::Display for Error<C, N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Card(error) => write!(formatter, "LPA card operation failed: {error}"),
            Self::Network(error) => write!(formatter, "LPA network operation failed: {error}"),
            Self::Authentication(reason) => {
                write!(formatter, "card authentication failed: {reason}")
            }
            Self::Invalid(reason) => write!(formatter, "invalid LPA session: {reason}"),
            Self::ConfirmationCodeRequired => formatter.write_str("confirmation code is required"),
            Self::Rejected => formatter.write_str("profile download was rejected"),
            Self::Cancelled => formatter.write_str("profile download was cancelled"),
            Self::TimedOut => formatter.write_str("profile download exceeded five minutes"),
            Self::CancelFailed { original, detail } => {
                write!(formatter, "{original}; cancellation also failed: {detail}")
            }
        }
    }
}

impl<C, N> std::error::Error for Error<C, N>
where
    C: fmt::Debug + fmt::Display,
    N: fmt::Debug + fmt::Display,
{
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadOutcome {
    pub transaction_id: Vec<u8>,
    pub isdp_aid: Vec<u8>,
}

pub struct Client<C, N> {
    card: C,
    network: N,
}

impl<C, N> Client<C, N>
where
    C: RspCard,
    N: Es9Transport,
{
    pub fn new(card: C, network: N) -> Self {
        Self { card, network }
    }

    pub fn card_mut(&mut self) -> &mut C {
        &mut self.card
    }

    pub fn into_parts(self) -> (C, N) {
        (self.card, self.network)
    }

    pub fn download(
        &mut self,
        activation: &ActivationCode,
        imei: &str,
        observer: &mut dyn DownloadObserver,
    ) -> Result<DownloadOutcome, Error<C::Error, N::Error>> {
        self.download_with_eid(activation, imei, "", observer)
    }

    /// Downloads a profile and supplies its EID to the reference-size
    /// estimator used by the pre-install confirmation callback.
    pub fn download_with_eid(
        &mut self,
        activation: &ActivationCode,
        imei: &str,
        eid: &str,
        observer: &mut dyn DownloadObserver,
    ) -> Result<DownloadOutcome, Error<C::Error, N::Error>> {
        self.download_with_eid_and_auth_hook(activation, imei, eid, observer, |_, _, _| Ok(()))
    }

    /// As [`Self::download_with_eid`], with a fail-closed card authentication
    /// hook invoked after the eUICC challenge is read and immediately before
    /// the standard AuthenticateServer APDU. The hook is suitable for
    /// per-card vendor authentication such as 9eSIM B0 BF71.
    pub fn download_with_eid_and_auth_hook<F>(
        &mut self,
        activation: &ActivationCode,
        imei: &str,
        eid: &str,
        observer: &mut dyn DownloadObserver,
        mut auth_hook: F,
    ) -> Result<DownloadOutcome, Error<C::Error, N::Error>>
    where
        F: FnMut(&mut C, &str, &[u8]) -> Result<(), String>,
    {
        let started = Instant::now();
        observer.on_stage(DownloadStage::InitiateAuthentication);
        if observer.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let challenge = self.card.challenge().map_err(Error::Card)?;
        let info1 = self.card.euicc_info1().map_err(Error::Card)?;
        let initiated = self
            .network
            .initiate(
                activation,
                &challenge,
                &info1,
                remaining_download_time(started)?,
            )
            .map_err(Error::Network)?;
        observer.on_stage(DownloadStage::AuthenticateEuicc);
        if let Err(error) = auth_hook(&mut self.card, eid, &challenge) {
            return match self.cancel_best_effort(
                activation,
                &initiated.transaction_id,
                &initiated.server_transaction_id,
                CancelSessionReason::MetadataMismatch,
            ) {
                Ok(()) => Err(Error::Authentication(error)),
                Err(detail) => Err(Error::CancelFailed {
                    original: "card pre-authentication failed",
                    detail: format!("{error}; {detail}"),
                }),
            };
        }
        let card_authentication = match self.card.authenticate_server(ServerAuthenticationRequest {
            signed1: &initiated.signed1,
            signature1: &initiated.signature1,
            euicc_ci_pk_id: &initiated.euicc_ci_pk_id,
            server_certificate: &initiated.server_certificate,
            matching_id: activation.matching_id().as_bytes(),
            imei,
        }) {
            Ok(response) => response,
            Err(error) => {
                return self.card_failure(
                    activation,
                    &initiated.transaction_id,
                    &initiated.server_transaction_id,
                    CancelSessionReason::MetadataMismatch,
                    "server authentication failed",
                    error,
                );
            }
        };
        let timeout = self.remaining_or_cancel(
            started,
            activation,
            &initiated.transaction_id,
            &initiated.server_transaction_id,
        )?;
        observer.on_stage(DownloadStage::AuthenticateClient);
        let authenticated = match self.network.authenticate_client(
            activation,
            &initiated.server_transaction_id,
            &card_authentication,
            timeout,
        ) {
            Ok(response) => response,
            Err(error) => {
                return self.network_failure(
                    activation,
                    &initiated.transaction_id,
                    &initiated.server_transaction_id,
                    CancelSessionReason::Postponed,
                    "client authentication failed",
                    observer,
                    error,
                );
            }
        };

        if observer.is_cancelled() {
            return self.abort(
                activation,
                &authenticated.transaction_id,
                &authenticated.server_transaction_id,
                CancelSessionReason::Postponed,
                "profile download was cancelled",
                Error::Cancelled,
            );
        }
        let metadata = match parse_profile_metadata(&authenticated.profile_metadata) {
            Ok(metadata) => metadata,
            Err(_) => {
                return self.abort(
                    activation,
                    &authenticated.transaction_id,
                    &authenticated.server_transaction_id,
                    CancelSessionReason::MetadataMismatch,
                    "invalid profile metadata",
                    Error::Invalid("invalid profile metadata"),
                );
            }
        };
        let estimated_size = estimate_profile_size(activation.server_address(), eid, &metadata);
        if !observer.confirm_profile_details(
            &authenticated.profile_metadata,
            &metadata,
            estimated_size,
        ) {
            return self.abort(
                activation,
                &authenticated.transaction_id,
                &authenticated.server_transaction_id,
                CancelSessionReason::Postponed,
                "profile download was rejected",
                Error::Rejected,
            );
        }

        let signed_confirmation_required =
            match proletarius_euicc::confirmation_code_required(&authenticated.signed2) {
                Ok(required) => required,
                Err(_) => {
                    return self.abort(
                        activation,
                        &authenticated.transaction_id,
                        &authenticated.server_transaction_id,
                        CancelSessionReason::MetadataMismatch,
                        "invalid confirmation-code indicator",
                        Error::Invalid("invalid confirmation-code indicator"),
                    );
                }
            };
        let confirmation_required =
            activation.confirmation_code_required() || signed_confirmation_required;
        let confirmation_hash = match (confirmation_required, activation.confirmation_code()) {
            (true, None) => {
                return self.abort(
                    activation,
                    &authenticated.transaction_id,
                    &authenticated.server_transaction_id,
                    CancelSessionReason::Postponed,
                    "confirmation code is required",
                    Error::ConfirmationCodeRequired,
                );
            }
            (true, Some(code)) => Some(hash_confirmation_code(
                code.as_bytes(),
                &authenticated.transaction_id,
            )),
            (false, _) => None,
        };

        observer.on_stage(DownloadStage::AuthenticateServer);
        if observer.is_cancelled() {
            return self.abort(
                activation,
                &authenticated.transaction_id,
                &authenticated.server_transaction_id,
                CancelSessionReason::Postponed,
                "profile download was cancelled",
                Error::Cancelled,
            );
        }
        let prepared = match self.card.prepare_download(PrepareDownloadRequest {
            signed2: &authenticated.signed2,
            signature2: &authenticated.signature2,
            server_certificate: &authenticated.server_certificate,
            confirmation_code_hash: confirmation_hash.as_ref().map(<[u8; 32]>::as_slice),
        }) {
            Ok(prepared) => prepared,
            Err(error) => {
                return self.card_failure(
                    activation,
                    &authenticated.transaction_id,
                    &authenticated.server_transaction_id,
                    CancelSessionReason::Postponed,
                    "prepare download failed",
                    error,
                );
            }
        };
        let timeout = self.remaining_or_cancel(
            started,
            activation,
            &authenticated.transaction_id,
            &authenticated.server_transaction_id,
        )?;
        let package = match self.network.get_bound_profile_package(
            activation,
            &authenticated.server_transaction_id,
            &prepared,
            timeout,
        ) {
            Ok(package) => package,
            Err(error) => {
                return self.network_failure(
                    activation,
                    &authenticated.transaction_id,
                    &authenticated.server_transaction_id,
                    CancelSessionReason::Postponed,
                    "bound profile package request failed",
                    observer,
                    error,
                );
            }
        };

        observer.on_stage(DownloadStage::Install);
        self.remaining_or_cancel(
            started,
            activation,
            &authenticated.transaction_id,
            &authenticated.server_transaction_id,
        )?;
        if observer.is_cancelled() {
            return self.abort(
                activation,
                &authenticated.transaction_id,
                &authenticated.server_transaction_id,
                CancelSessionReason::Postponed,
                "profile download was cancelled",
                Error::Cancelled,
            );
        }
        let installed = match self.card.install(&package.package) {
            Ok(installed) => installed,
            Err(error) => {
                if let Err(detail) = self.cancel_best_effort(
                    activation,
                    &package.transaction_id,
                    &package.server_transaction_id,
                    CancelSessionReason::LoadBppExecutionError,
                ) {
                    return Err(Error::CancelFailed {
                        original: "profile installation failed",
                        detail: format!("{error}; {detail}"),
                    });
                }
                return Err(Error::Card(error));
            }
        };

        observer.on_stage(DownloadStage::Done);
        Ok(DownloadOutcome {
            transaction_id: installed.transaction_id,
            isdp_aid: installed.isdp_aid,
        })
    }

    /// Retry exactly one card notification.  Removal happens only after a
    /// successful ES9+ response.
    pub fn retry_notification(
        &mut self,
        sequence_number: u64,
    ) -> Result<(), Error<C::Error, N::Error>> {
        let notification = self
            .card
            .retrieve_notification(sequence_number)
            .map_err(Error::Card)?
            .ok_or(Error::Invalid("notification does not exist"))?;
        self.network
            .handle_notification(
                &notification.address,
                &notification.encoded,
                NOTIFICATION_TIMEOUT,
            )
            .map_err(Error::Network)?;
        self.card
            .remove_notification(sequence_number)
            .map_err(Error::Card)?;
        Ok(())
    }

    fn abort<T>(
        &mut self,
        activation: &ActivationCode,
        transaction_id: &[u8],
        server_transaction_id: &str,
        reason: CancelSessionReason,
        original: &'static str,
        result: Error<C::Error, N::Error>,
    ) -> Result<T, Error<C::Error, N::Error>> {
        match self.cancel_best_effort(activation, transaction_id, server_transaction_id, reason) {
            Ok(()) => Err(result),
            Err(detail) => Err(Error::CancelFailed { original, detail }),
        }
    }

    fn cancel_best_effort(
        &mut self,
        activation: &ActivationCode,
        transaction_id: &[u8],
        server_transaction_id: &str,
        reason: CancelSessionReason,
    ) -> Result<(), String> {
        let response = self
            .card
            .cancel(transaction_id, reason)
            .map_err(|error| error.to_string())?;
        self.network
            .cancel_session(activation, server_transaction_id, &response, CANCEL_TIMEOUT)
            .map_err(|error| error.to_string())
    }

    fn remaining_or_cancel(
        &mut self,
        started: Instant,
        activation: &ActivationCode,
        transaction_id: &[u8],
        server_transaction_id: &str,
    ) -> Result<Duration, Error<C::Error, N::Error>> {
        match remaining_download_time::<C::Error, N::Error>(started) {
            Ok(remaining) => Ok(remaining),
            Err(Error::TimedOut) => self.abort(
                activation,
                transaction_id,
                server_transaction_id,
                CancelSessionReason::Timeout,
                "profile download exceeded five minutes",
                Error::TimedOut,
            ),
            Err(_) => unreachable!("remaining_download_time returns only TimedOut"),
        }
    }

    fn card_failure<T>(
        &mut self,
        activation: &ActivationCode,
        transaction_id: &[u8],
        server_transaction_id: &str,
        reason: CancelSessionReason,
        original: &'static str,
        error: C::Error,
    ) -> Result<T, Error<C::Error, N::Error>> {
        match self.cancel_best_effort(activation, transaction_id, server_transaction_id, reason) {
            Ok(()) => Err(Error::Card(error)),
            Err(detail) => Err(Error::CancelFailed {
                original,
                detail: format!("{error}; {detail}"),
            }),
        }
    }

    fn network_failure<T>(
        &mut self,
        activation: &ActivationCode,
        transaction_id: &[u8],
        server_transaction_id: &str,
        reason: CancelSessionReason,
        _original: &'static str,
        observer: &mut dyn DownloadObserver,
        error: N::Error,
    ) -> Result<T, Error<C::Error, N::Error>> {
        if let Err(detail) =
            self.cancel_best_effort(activation, transaction_id, server_transaction_id, reason)
        {
            observer.on_warning(&format!(
                "ES9+ primary network failure preserved; cancellation also failed: {detail}"
            ));
        }
        Err(Error::Network(error))
    }
}

fn remaining_download_time<C, N>(started: Instant) -> Result<Duration, Error<C, N>> {
    MAX_DOWNLOAD_DURATION
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(Error::TimedOut)
}

fn hash_confirmation_code(code: &[u8], transaction_id: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(code);
    let mut digest = Sha256::new();
    digest.update(first);
    digest.update(transaction_id);
    digest.finalize().into()
}

#[cfg(test)]
mod tests;
