// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

//! Transport-independent ES9+ messages used by the LPA state machine.
//!
//! The Switch HTTP client implements [`Es9Transport`] separately so the LPA
//! flow cannot silently choose proxy, certificate, redirect, or timeout policy.

use std::{fmt, time::Duration};

use crate::ActivationCode;

#[derive(Debug)]
pub enum HttpError {
    Transport(String),
    Json(String),
    Base64(String),
    Remote {
        status: String,
        subject_code: Option<String>,
        reason_code: Option<String>,
        message: Option<String>,
        subject_identifier: Option<String>,
    },
    Status(u16),
    Invalid(&'static str),
    TooLarge,
}

impl fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "ES9+ transport error: {error}"),
            Self::Json(error) => write!(formatter, "invalid ES9+ JSON: {error}"),
            Self::Base64(error) => write!(formatter, "invalid ES9+ base64 payload: {error}"),
            Self::Remote {
                status,
                subject_code,
                reason_code,
                message,
                subject_identifier,
            } => write!(
                formatter,
                "ES9+ failed ({status}, subject={}, reason={}, identifier={}): {}",
                subject_code.as_deref().unwrap_or("unknown"),
                reason_code.as_deref().unwrap_or("unknown"),
                subject_identifier.as_deref().unwrap_or("unknown"),
                message.as_deref().unwrap_or("no message")
            ),
            Self::Status(status) => write!(formatter, "ES9+ HTTP status {status}"),
            Self::Invalid(reason) => write!(formatter, "invalid ES9+ response: {reason}"),
            Self::TooLarge => formatter.write_str("ES9+ response exceeds the size limit"),
        }
    }
}

impl std::error::Error for HttpError {}

pub trait Es9Transport {
    type Error: fmt::Display;

    fn initiate(
        &self,
        activation: &ActivationCode,
        challenge: &[u8],
        euicc_info1: &[u8],
        timeout: Duration,
    ) -> Result<InitiateResponse, Self::Error>;

    fn authenticate_client(
        &self,
        activation: &ActivationCode,
        server_transaction_id: &str,
        authenticate_server_response: &[u8],
        timeout: Duration,
    ) -> Result<AuthenticateResponse, Self::Error>;

    fn get_bound_profile_package(
        &self,
        activation: &ActivationCode,
        server_transaction_id: &str,
        prepare_download_response: &[u8],
        timeout: Duration,
    ) -> Result<BoundProfilePackage, Self::Error>;

    fn handle_notification(
        &self,
        address: &str,
        notification: &[u8],
        timeout: Duration,
    ) -> Result<(), Self::Error>;

    fn cancel_session(
        &self,
        activation: &ActivationCode,
        server_transaction_id: &str,
        cancel_response: &[u8],
        timeout: Duration,
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitiateResponse {
    pub server_transaction_id: String,
    pub transaction_id: Vec<u8>,
    pub signed1: Vec<u8>,
    pub signature1: Vec<u8>,
    pub euicc_ci_pk_id: Vec<u8>,
    pub server_certificate: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticateResponse {
    pub server_transaction_id: String,
    pub transaction_id: Vec<u8>,
    pub profile_metadata: Vec<u8>,
    pub signed2: Vec<u8>,
    pub signature2: Vec<u8>,
    pub server_certificate: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundProfilePackage {
    pub server_transaction_id: String,
    pub transaction_id: Vec<u8>,
    pub package: Vec<u8>,
}
