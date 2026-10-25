use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbDeviceId {
    pub vendor_id: u16,
    pub product_id: u16,
}

pub const DEFAULT_DEVICE_IDS: [UsbDeviceId; 2] = [
    UsbDeviceId {
        vendor_id: 0x2ca3,
        product_id: 0x4006,
    },
    UsbDeviceId {
        vendor_id: 0x2c7c,
        product_id: 0x0125,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsbSelector {
    PriorityList(Vec<UsbDeviceId>),
    Exact(UsbDeviceId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectorError(pub String);

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SelectorError {}

impl UsbSelector {
    /// Parses the same auto and VID:PID spellings accepted by Switch's Go
    /// libusb QMI owner.
    pub fn parse(path: &str) -> Result<Self, SelectorError> {
        let normalized = path.trim();
        if normalized.is_empty() || normalized.eq_ignore_ascii_case("auto") {
            return Ok(Self::PriorityList(DEFAULT_DEVICE_IDS.to_vec()));
        }
        let (vendor, product) = normalized.split_once(':').ok_or_else(|| {
            SelectorError(format!(
                "invalid QMI libusb path {path:?} (want VID:PID, e.g. 2ca3:4006)"
            ))
        })?;
        if product.contains(':') {
            return Err(SelectorError(format!(
                "invalid QMI libusb path {path:?} (want VID:PID, e.g. 2ca3:4006)"
            )));
        }
        let vendor_id = parse_hex_u16(vendor)
            .map_err(|error| SelectorError(format!("invalid VID in path {path:?}: {error}")))?;
        let product_id = parse_hex_u16(product)
            .map_err(|error| SelectorError(format!("invalid PID in path {path:?}: {error}")))?;
        Ok(Self::Exact(UsbDeviceId {
            vendor_id,
            product_id,
        }))
    }

    pub fn device_ids(&self) -> &[UsbDeviceId] {
        match self {
            Self::PriorityList(ids) => ids,
            Self::Exact(id) => std::slice::from_ref(id),
        }
    }
}

fn parse_hex_u16(input: &str) -> Result<u16, String> {
    let digits = input.trim();
    let digits = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .unwrap_or(digits);
    if digits.is_empty() {
        return Err("empty hex value".to_owned());
    }
    u16::from_str_radix(digits, 16).map_err(|error| match error.kind() {
        std::num::IntErrorKind::PosOverflow => "value out of uint16 range".to_owned(),
        _ => format!("invalid hexadecimal value {input:?}"),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointType {
    Bulk,
    Interrupt,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbEndpoint {
    pub address: u8,
    pub transfer_type: EndpointType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QmiInterfaceCandidate {
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoint_in: u8,
    pub endpoint_out: u8,
}

/// Mirrors the Go owner's device-aware candidate filter. The DJI composition
/// requires its known ff/ff/ff interface signature and an interrupt endpoint;
/// other supported products accept vendor-specific bulk interfaces except
/// ADB ff/42/01.
pub fn qmi_interface_candidate(
    device: UsbDeviceId,
    interface_number: u8,
    alternate_setting: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    endpoints: &[UsbEndpoint],
) -> Option<QmiInterfaceCandidate> {
    if class != 0xff || (subclass == 0x42 && protocol == 0x01) {
        return None;
    }

    let mut bulk_in = 0;
    let mut bulk_out = 0;
    let mut has_interrupt = false;
    for endpoint in endpoints {
        match endpoint.transfer_type {
            EndpointType::Interrupt => has_interrupt = true,
            EndpointType::Bulk if endpoint.address & 0x80 != 0 => {
                bulk_in = endpoint.address;
            }
            EndpointType::Bulk => bulk_out = endpoint.address,
            EndpointType::Other => {}
        }
    }
    if bulk_in == 0 || bulk_out == 0 {
        return None;
    }

    if device == DEFAULT_DEVICE_IDS[0] && (subclass != 0xff || protocol != 0xff || !has_interrupt) {
        return None;
    }

    Some(QmiInterfaceCandidate {
        interface_number,
        alternate_setting,
        class,
        subclass,
        protocol,
        endpoint_in: bulk_in,
        endpoint_out: bulk_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_matches_auto_explicit_and_hex_prefix_contract() {
        assert_eq!(
            UsbSelector::parse(" auto ").unwrap(),
            UsbSelector::PriorityList(DEFAULT_DEVICE_IDS.to_vec())
        );
        assert_eq!(
            UsbSelector::parse("0x2CA3:0x4006").unwrap(),
            UsbSelector::Exact(DEFAULT_DEVICE_IDS[0])
        );
    }

    #[test]
    fn selector_rejects_invalid_shape_digits_and_overflow() {
        for invalid in ["2ca3", "2ca3:4006:1", "gggg:4006", "10000:4006"] {
            assert!(UsbSelector::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn dji_filter_requires_known_signature_interrupt_and_bulk_directions() {
        let endpoints = [
            UsbEndpoint {
                address: 0x81,
                transfer_type: EndpointType::Bulk,
            },
            UsbEndpoint {
                address: 0x02,
                transfer_type: EndpointType::Bulk,
            },
            UsbEndpoint {
                address: 0x83,
                transfer_type: EndpointType::Interrupt,
            },
        ];
        let accepted =
            qmi_interface_candidate(DEFAULT_DEVICE_IDS[0], 4, 0, 0xff, 0xff, 0xff, &endpoints);
        assert_eq!(accepted.unwrap().endpoint_in, 0x81);
        assert!(
            qmi_interface_candidate(DEFAULT_DEVICE_IDS[0], 4, 0, 0xff, 0, 0, &endpoints[..2])
                .is_none()
        );
    }

    #[test]
    fn adb_is_rejected_and_quectel_vendor_bulk_interface_is_accepted() {
        let endpoints = [
            UsbEndpoint {
                address: 0x84,
                transfer_type: EndpointType::Bulk,
            },
            UsbEndpoint {
                address: 0x05,
                transfer_type: EndpointType::Bulk,
            },
        ];
        assert!(
            qmi_interface_candidate(DEFAULT_DEVICE_IDS[1], 6, 0, 0xff, 0x42, 0x01, &endpoints)
                .is_none()
        );
        assert_eq!(
            qmi_interface_candidate(DEFAULT_DEVICE_IDS[1], 7, 0, 0xff, 0, 0, &endpoints)
                .unwrap()
                .endpoint_out,
            0x05
        );
    }
}
