// Copyright (c) 2026 anti-rainer
// SPDX-License-Identifier: MIT

use std::{fmt, str::FromStr};
use zeroize::Zeroize;

pub const MAX_ACTIVATION_CODE_BYTES: usize = 1024;
pub const MAX_SERVER_ADDRESS_BYTES: usize = 253;
pub const MAX_MATCHING_ID_BYTES: usize = 255;
pub const MAX_OID_BYTES: usize = 255;
pub const MAX_CONFIRMATION_CODE_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationCode {
    server_address: String,
    matching_id: String,
    oid: Option<String>,
    confirmation_code_required: bool,
    confirmation_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationCodeError {
    Empty,
    TooLarge,
    UnsupportedVersion,
    MissingServer,
    InvalidServer,
    InvalidField,
    UnexpectedField,
}

impl fmt::Display for ActivationCodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "activation code is empty",
            Self::TooLarge => "activation code exceeds the size limit",
            Self::UnsupportedVersion => "activation code is not LPA version 1",
            Self::MissingServer => "activation code has no SM-DP+ address",
            Self::InvalidServer => "activation code SM-DP+ address is invalid",
            Self::InvalidField => "activation code contains an invalid field",
            Self::UnexpectedField => "activation code has too many fields",
        })
    }
}

impl std::error::Error for ActivationCodeError {}

impl ActivationCode {
    pub fn parse(input: &str) -> Result<Self, ActivationCodeError> {
        input.parse()
    }

    #[must_use]
    pub fn server_address(&self) -> &str {
        &self.server_address
    }

    #[must_use]
    pub fn matching_id(&self) -> &str {
        &self.matching_id
    }

    #[must_use]
    pub fn oid(&self) -> Option<&str> {
        self.oid.as_deref()
    }

    #[must_use]
    pub const fn confirmation_code_required(&self) -> bool {
        self.confirmation_code_required
    }

    #[must_use]
    pub fn confirmation_code(&self) -> Option<&str> {
        self.confirmation_code.as_deref()
    }

    pub fn set_confirmation_code(
        &mut self,
        confirmation_code: Option<&str>,
    ) -> Result<(), ActivationCodeError> {
        if let Some(previous) = self.confirmation_code.as_mut() {
            previous.zeroize();
        }
        self.confirmation_code = confirmation_code
            .map(validate_confirmation_code)
            .transpose()?
            .map(String::from);
        Ok(())
    }
}

impl Drop for ActivationCode {
    fn drop(&mut self) {
        self.server_address.zeroize();
        self.matching_id.zeroize();
        if let Some(oid) = self.oid.as_mut() {
            oid.zeroize();
        }
        if let Some(code) = self.confirmation_code.as_mut() {
            code.zeroize();
        }
    }
}

impl FromStr for ActivationCode {
    type Err = ActivationCodeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        // A code arrives by paste as often as by QR scan, so it carries
        // whatever the clipboard held around it, and operators print it both
        // with and without the `LPA:` prefix - a card shows `LPA:1$...` while
        // an email usually shows the bare `1$...` form. The version field is
        // what the syntax is defined by, so neither difference is an error.
        let input = input.trim();
        if input.is_empty() {
            return Err(ActivationCodeError::Empty);
        }
        if input.len() > MAX_ACTIVATION_CODE_BYTES {
            return Err(ActivationCodeError::TooLarge);
        }
        if input.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(ActivationCodeError::InvalidField);
        }
        let input = match input.get(..4) {
            Some(prefix) if prefix.eq_ignore_ascii_case("LPA:") => &input[4..],
            _ => input,
        };
        let mut parts = input.split('$');
        if parts.next() != Some("1") {
            return Err(ActivationCodeError::UnsupportedVersion);
        }
        let server = parts.next().ok_or(ActivationCodeError::MissingServer)?;
        let matching_id = parts.next().unwrap_or_default();
        let oid = parts.next();
        let confirmation_required = parts.next();
        if parts.next().is_some() {
            return Err(ActivationCodeError::UnexpectedField);
        }
        let server_address = validate_server(server)?;
        validate_field(matching_id, MAX_MATCHING_ID_BYTES)?;
        let oid = oid
            .filter(|value| !value.is_empty())
            .map(|value| validate_field(value, MAX_OID_BYTES))
            .transpose()?
            .map(String::from);
        let confirmation_code_required = match confirmation_required {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err(ActivationCodeError::InvalidField),
        };
        Ok(Self {
            server_address,
            matching_id: String::from(matching_id),
            oid,
            confirmation_code_required,
            confirmation_code: None,
        })
    }
}

impl fmt::Display for ActivationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LPA:1${}${}",
            self.server_address, self.matching_id
        )?;
        if self.oid.is_some() || self.confirmation_code_required {
            write!(formatter, "${}", self.oid.as_deref().unwrap_or_default())?;
        }
        if self.confirmation_code_required {
            formatter.write_str("$1")?;
        }
        Ok(())
    }
}

fn validate_server(input: &str) -> Result<String, ActivationCodeError> {
    if input.is_empty() {
        return Err(ActivationCodeError::MissingServer);
    }
    if input.len() > MAX_SERVER_ADDRESS_BYTES
        || input.contains('/')
        || input.contains('@')
        || input.contains('?')
        || input.contains('#')
    {
        return Err(ActivationCodeError::InvalidServer);
    }
    let parsed = format!("https://{input}/")
        .parse::<http::Uri>()
        .map_err(|_| ActivationCodeError::InvalidServer)?;
    if parsed.scheme_str() != Some("https")
        || parsed.authority().is_none()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed
            .authority()
            .is_some_and(|value| value.as_str().contains('@'))
    {
        return Err(ActivationCodeError::InvalidServer);
    }
    Ok(String::from(input))
}

fn validate_field(input: &str, limit: usize) -> Result<&str, ActivationCodeError> {
    if input.len() > limit
        || input.contains('$')
        || input.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ActivationCodeError::InvalidField);
    }
    Ok(input)
}

fn validate_confirmation_code(input: &str) -> Result<&str, ActivationCodeError> {
    if input.is_empty() {
        return Err(ActivationCodeError::InvalidField);
    }
    validate_field(input, MAX_CONFIRMATION_CODE_BYTES)
}
