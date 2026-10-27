//! CLCC voice classification shared by UI projection and admission.
//!
//! Quectel EC2x/EG9x/EG2x-G/EM05 AT Commands Manual V2.0,
//! pp. 114 and 127-128: mode 0 is voice; mode 1 is data, including
//! the LTE PS row +CLCC: 1,0,0,1,0,"",128. A missing phone number
//! does not prove that a mode-0 voice call ended.
use super::CallState;

const MAX_BYTES: usize = 64 * 1024;
const MAX_ROWS: usize = 64;
const MAX_FIELD_BYTES: usize = 512;

pub(super) struct Observation {
    pub(super) display: CallState,
    pub(super) safe_voice_idle: bool,
}

#[derive(Debug)]
struct Field {
    value: String,
    quoted: bool,
}
#[derive(Debug)]
struct Row {
    status: u32,
    mode: u32,
    number: String,
    // Historical six-field voice samples omit the optional address type.
    // They can still describe a live voice call, but never prove idle.
    complete: bool,
}

fn decimal(field: &Field) -> Option<u32> {
    (!field.quoted
        && !field.value.is_empty()
        && field.value.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| field.value.parse().ok())
    .flatten()
}

fn fields(text: &str) -> Option<Vec<Field>> {
    let mut result = Vec::new();
    let mut value = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut closed = false;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    value.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                if character.is_control() {
                    return None;
                }
                value.push(character);
            }
        } else if character == ',' {
            result.push(Field {
                value: if was_quoted {
                    value
                } else {
                    value.trim().into()
                },
                quoted: was_quoted,
            });
            if result.len() >= 8 {
                return None;
            }
            value = String::new();
            was_quoted = false;
            closed = false;
        } else if closed {
            if !matches!(character, ' ' | '\t') {
                return None;
            }
        } else if character == '"' {
            if !value.trim().is_empty() {
                return None;
            }
            value.clear();
            quoted = true;
            was_quoted = true;
        } else {
            if character.is_control() && character != '\t' {
                return None;
            }
            value.push(character);
        }
        if value.len() > MAX_FIELD_BYTES {
            return None;
        }
    }
    if quoted {
        return None;
    }
    result.push(Field {
        value: if was_quoted {
            value
        } else {
            value.trim().into()
        },
        quoted: was_quoted,
    });
    Some(result)
}

fn row(text: &str) -> Option<Row> {
    let fields = fields(text)?;
    if !(5..=8).contains(&fields.len()) {
        return None;
    }
    let _id = decimal(&fields[0]).filter(|id| *id > 0)?;
    let _direction = decimal(&fields[1]).filter(|direction| *direction <= 1)?;
    let status = decimal(&fields[2]).filter(|status| *status <= 6)?;
    let mode = decimal(&fields[3])?;
    let _multiparty = decimal(&fields[4]).filter(|multiparty| *multiparty <= 1)?;
    if fields.len() >= 7 {
        decimal(&fields[6]).filter(|kind| *kind <= 255)?;
    }
    Some(Row {
        status,
        mode,
        number: fields
            .get(5)
            .map_or_else(String::new, |field| field.value.clone()),
        complete: fields.len() != 6,
    })
}

pub(super) fn observe(response: &str) -> Observation {
    let unknown = || Observation {
        display: CallState {
            state: "unknown".into(),
            ..CallState::default()
        },
        safe_voice_idle: false,
    };
    if response.len() > MAX_BYTES {
        return unknown();
    }
    let mut valid = true;
    let mut terminal = None;
    let mut terminal_count = 0;
    let mut row_count = 0;
    let mut voice_present = false;
    let mut incoming = false;
    let mut active = false;
    let mut selected: Option<Row> = None;
    for line in response
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        terminal = Some(line);
        if line == "OK" {
            terminal_count += 1;
            continue;
        }
        if line == "AT+CLCC" {
            continue;
        }
        let Some(text) = line.strip_prefix("+CLCC:") else {
            // Errors, call URCs and unrecognized/partial lines cannot establish
            // voice-idle. In particular, a trailing RING must not clear a call.
            valid = false;
            continue;
        };
        row_count += 1;
        if row_count > MAX_ROWS {
            return unknown();
        }
        let Some(row) = row(text) else {
            valid = false;
            continue;
        };
        if row.mode == 1 {
            valid &= row.complete;
            continue;
        }
        if row.mode != 0 {
            // Fax, unknown and vendor-specific modes are not silently treated
            // as idle. Only the explicit data mode is excluded this way.
            valid = false;
            continue;
        }
        voice_present = true;
        incoming |= matches!(row.status, 4 | 5);
        active |= matches!(row.status, 0 | 1);
        if matches!(row.status, 0..=5) {
            // Preserve the established last-live-row display choice, but only
            // actual voice rows may supply that state or phone number.
            selected = Some(row);
        }
    }
    let complete = terminal == Some("OK") && terminal_count == 1;
    let safe_voice_idle = valid && complete && !voice_present;
    let display = if let Some(row) = selected {
        CallState {
            state: match row.status {
                4 | 5 => "incoming",
                0 | 1 => "active",
                3 => "alerting",
                2 => "dialing",
                _ => unreachable!("only live voice rows are selected"),
            }
            .into(),
            number: row.number,
            incoming,
            active,
        }
    } else {
        CallState {
            state: if safe_voice_idle { "idle" } else { "unknown" }.into(),
            ..CallState::default()
        }
    };
    Observation {
        display,
        safe_voice_idle,
    }
}

#[cfg(test)]
mod tests;
