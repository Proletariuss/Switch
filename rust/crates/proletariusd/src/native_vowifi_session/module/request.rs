use super::*;
use serde::Deserialize;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Decoded {
    pub(super) enable: bool,
    pub(super) connection: intent::FrozenRequest,
}

pub(super) fn automatic() -> intent::FrozenRequest {
    intent::FrozenRequest {
        proxy: String::new(),
        epdg: String::new(),
        epdg_port: 0,
    }
}

fn fields(bytes: &[u8]) -> Result<api::Fields, &'static str> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Option::<api::Fields>::deserialize(&mut decoder)
        .map_err(|_| "invalid_vowifi_request")?
        .ok_or("invalid_vowifi_request")
}

/// Preserve the public decoder's ordered, case-folded Go pointer semantics:
/// null clears a prior value, later booleans replace it, and any invalid type
/// rejects the request even if another duplicate would contain a valid bool.
fn requested_enable_from_fields(fields: &api::Fields) -> Result<bool, &'static str> {
    let mut enable = None;
    for (key, value) in &fields.0 {
        if api::field_name(key) == "enable" {
            enable = if value.is_null() {
                None
            } else {
                Some(value.as_bool().ok_or("invalid_vowifi_request")?)
            };
        }
    }
    enable.ok_or("invalid_vowifi_request")
}

/// Recover only the requested boolean, without accepting or normalizing the
/// connection fields of an otherwise invalid request.
pub(super) fn requested_enable(bytes: &[u8]) -> Result<bool, &'static str> {
    requested_enable_from_fields(&fields(bytes)?)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Decoded, &'static str> {
    let fields = fields(bytes)?;
    let enable = requested_enable_from_fields(&fields)?;
    let mut proxy = String::new();
    let mut proxy_host = None;
    let mut proxy_port = None;
    let mut epdg = String::new();
    let mut epdg_ip = String::new();
    let mut epdg_port = 0_i64;
    let mut epdg_port_text = None;
    for (key, value) in fields.0 {
        match api::field_name(&key).as_str() {
            // Enable is interpreted once above by the same helper used for
            // rejection and reconciliation. Other fields retain legacy order.
            "enable" => {}
            "wifi_calling_mode" if !value.is_null() => {
                value.as_i64().ok_or("invalid_vowifi_request")?;
            }
            "epdg_auto_detect" if !value.is_null() => {
                value.as_bool().ok_or("invalid_vowifi_request")?;
            }
            "proxy" if !value.is_null() => {
                proxy = value
                    .as_str()
                    .ok_or("invalid_vowifi_request")?
                    .trim()
                    .to_owned();
            }
            "proxy_host" if !value.is_null() => {
                proxy_host = Some(value.as_str().ok_or("invalid_vowifi_request")?.to_owned());
            }
            "proxy_port" if !value.is_null() => {
                proxy_port = Some(value.as_str().ok_or("invalid_vowifi_request")?.to_owned());
            }
            "epdg" | "epdg_address" if !value.is_null() => {
                epdg = value
                    .as_str()
                    .ok_or("invalid_vowifi_request")?
                    .trim()
                    .to_owned();
            }
            // This legacy field is a numeric endpoint pin, while the native
            // PlanRequest deliberately accepts an FQDN so proxy-owned DNS and
            // certificate identity remain intact. Keep it fail-closed.
            "epdg_ip_address" if !value.is_null() => {
                epdg_ip = value
                    .as_str()
                    .ok_or("invalid_vowifi_request")?
                    .trim()
                    .to_owned();
            }
            "epdg_port" if !value.is_null() => {
                epdg_port = value.as_i64().ok_or("invalid_vowifi_request")?;
            }
            "epdg_port_text" if !value.is_null() => {
                epdg_port_text = Some(value.as_str().ok_or("invalid_vowifi_request")?.to_owned());
            }
            _ => {}
        }
    }
    if !enable {
        return Ok(Decoded {
            enable,
            connection: automatic(),
        });
    }
    if !epdg_ip.is_empty() {
        return Err("module_epdg_ip_unsupported");
    }
    let proxy =
        crate::vowifi_input::normalize_proxy(&proxy, proxy_host.as_deref(), proxy_port.as_deref())?;
    let epdg = crate::vowifi_input::normalize_epdg(&epdg)?;
    let epdg_port = crate::vowifi_input::normalize_epdg_port(epdg_port, epdg_port_text.as_deref())?;
    let epdg_port = if epdg_port <= 0 {
        0
    } else {
        u16::try_from(epdg_port).map_err(|_| "ePDG 端口无效（1-65535）")?
    };
    Ok(Decoded {
        enable,
        connection: intent::FrozenRequest {
            proxy,
            epdg,
            epdg_port,
        },
    })
}
