use crate::{address, timestamp};

#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryStatus {
    Delivered,
    TemporaryError,
    PermanentError,
    TemporaryErrorNoRetry,
    Reserved,
}

impl DeliveryStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::TemporaryError => "temporary_error",
            Self::PermanentError => "permanent_error",
            Self::TemporaryErrorNoRetry => "temporary_error_no_retry",
            Self::Reserved => "reserved",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub reference: u8,
    pub recipient: String,
    pub service_center_time: Option<timestamp::Timestamp>,
    pub discharge_time: Option<timestamp::Timestamp>,
    pub status_code: u8,
    pub status: DeliveryStatus,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Framing,
    Address,
}

pub fn parse(tpdu: &[u8]) -> Result<Report, Error> {
    if tpdu.len() < 20 || tpdu.len() > 232 || tpdu[0] & 3 != 2 {
        return Err(Error::Framing);
    }
    let reference = tpdu[1];
    let (recipient, offset) = address::parse(tpdu, 2).map_err(|error| match error {
        address::Error::Framing => Error::Framing,
        address::Error::Invalid => Error::Address,
    })?;
    let fixed = tpdu.get(offset..offset + 15).ok_or(Error::Framing)?;
    let status_code = fixed[14];
    let status = match status_code {
        0x00..=0x1f => DeliveryStatus::Delivered,
        0x20..=0x3f => DeliveryStatus::TemporaryError,
        0x40..=0x5f => DeliveryStatus::PermanentError,
        0x60..=0x7f => DeliveryStatus::TemporaryErrorNoRetry,
        _ => DeliveryStatus::Reserved,
    };
    // Optional TP-PI fields are not required for delivery correlation. Preserve
    // compatibility with the previous bounded decoder by accepting them only
    // inside the already enforced 232-byte TPDU limit.
    Ok(Report {
        reference,
        recipient,
        service_center_time: timestamp::parse(&fixed[..7]).ok(),
        discharge_time: timestamp::parse(&fixed[7..14]).ok(),
        status_code,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &[u8] = &[
        0x02, 0x01, 0x04, 0x91, 0x44, 0x77, 0x62, 0x80, 0x62, 0x61, 0x21, 0, 0, 0x62, 0x80, 0x62,
        0x61, 0x21, 0, 0, 0,
    ];

    #[test]
    fn parses_successful_report_and_timestamps() {
        let report = parse(REPORT).unwrap();
        assert_eq!(report.reference, 1);
        assert_eq!(report.recipient, "+4477");
        assert_eq!(report.status, DeliveryStatus::Delivered);
        assert_eq!(report.service_center_time.unwrap().year, 2026);
        assert_eq!(report.discharge_time.unwrap().minute, 12);
    }

    #[test]
    fn maps_every_status_octet_and_rejects_all_truncations() {
        for (code, expected) in [
            (0x1f, DeliveryStatus::Delivered),
            (0x20, DeliveryStatus::TemporaryError),
            (0x40, DeliveryStatus::PermanentError),
            (0x60, DeliveryStatus::TemporaryErrorNoRetry),
            (0x80, DeliveryStatus::Reserved),
        ] {
            let mut value = REPORT.to_vec();
            *value.last_mut().unwrap() = code;
            assert_eq!(parse(&value).unwrap().status, expected);
        }
        for length in 0..REPORT.len() {
            assert!(parse(&REPORT[..length]).is_err());
        }
    }
}
