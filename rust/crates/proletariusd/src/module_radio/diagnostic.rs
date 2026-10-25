//! Console grammar and RF policy are validated again at the owning service.
use super::Failure;
use std::sync::OnceLock;

pub(super) fn parse_console_command(raw: &str) -> Result<String, &'static str> {
    if raw.len() > 4096 {
        return Err("AT 命令不能超过 4096 字节");
    }
    static INVISIBLE: OnceLock<regex::Regex> = OnceLock::new();
    if INVISIBLE
        .get_or_init(|| regex::Regex::new(r"[\p{Cc}\p{Cf}]").unwrap())
        .is_match(raw)
        || raw
            .chars()
            .any(|character| character.is_whitespace() && character != ' ')
    {
        return Err("AT 命令必须为单行，不能含换行、制表符或控制字符");
    }
    let text = raw.trim_matches(' ');
    if !text
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AT"))
    {
        return Err("命令必须以 AT 开头");
    }
    if !tokens(text, true).1 {
        return Err("AT 命令的引号未闭合");
    }
    Ok(text.to_owned())
}

pub(super) fn blocked(raw: &str) -> Result<bool, Failure> {
    let text = parse_console_command(raw).map_err(|_| Failure::invalid())?;
    let (mut parts, closed) = tokens(&text, true);
    if !closed {
        return Err(Failure::invalid());
    }
    if text.contains("\\\"") {
        parts.extend(tokens(&text, false).0);
    }
    Ok(parts.iter().any(|part| {
        let normalized = part.replace(' ', "").to_ascii_uppercase();
        matches!(
            normalized.as_str(),
            "AT+CSQ"
                | "AT+CESQ"
                | "AT+QCSQ"
                | "AT+CEREG?"
                | "AT+CREG?"
                | "AT+CGREG?"
                | "AT+COPS?"
                | "AT+COPS=?"
                | "AT+QNWINFO"
                | "AT+CGATT=1"
        ) || ["AT+COPS=", "AT+QENG", "AT+QSCAN"]
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
            || normalized
                .strip_prefix("AT+CFUN=")
                .is_some_and(|value| value != "4" && value != "?")
    }))
}

pub(super) fn result(response: &str) -> serde_json::Value {
    let normalized = response.replace('\r', "\n");
    let error_kind = normalized
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| {
            let line = line.to_ascii_uppercase();
            if line == "ERROR" {
                Some("generic")
            } else if line.starts_with("+CME ERROR:") {
                Some("cme")
            } else if line.starts_with("+CMS ERROR:") {
                Some("cms")
            } else {
                None
            }
        });
    match error_kind {
        Some(kind) => serde_json::json!({
            "response":response,
            "outcome":"module_error",
            "error_kind":kind
        }),
        None => serde_json::json!({"response":response,"outcome":"success"}),
    }
}

fn tokens(text: &str, escapes: bool) -> (Vec<String>, bool) {
    let (mut result, mut token) = (Vec::new(), String::new());
    let (mut quoted, mut escaped) = (false, false);
    for c in text.chars() {
        if quoted {
            if escaped {
                escaped = false;
                continue;
            }
            if escapes && c == '\\' {
                escaped = true;
                continue;
            }
            if c == '"' {
                quoted = false;
                token.push('"');
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                token.push_str("\"…");
            }
            ';' => append(&mut token, &mut result),
            _ => token.push(c),
        }
    }
    append(&mut token, &mut result);
    (result, !quoted)
}
fn append(token: &mut String, result: &mut Vec<String>) {
    let mut part = token.trim().to_owned();
    token.clear();
    if part.is_empty() {
        return;
    }
    if !part
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AT"))
    {
        part.insert_str(0, "AT");
    }
    let body = part[2..].trim();
    let suffix = if body
        .as_bytes()
        .first()
        .is_some_and(|b| b"+^%$#".contains(b))
    {
        None
    } else {
        body.char_indices().skip(1).find_map(|(i, c)| {
            if c.is_ascii()
                && b"+^%$#".contains(&(c as u8))
                && body
                    .as_bytes()
                    .get(i + 1)
                    .is_some_and(u8::is_ascii_alphabetic)
            {
                Some(format!("AT{}", &body[i..]))
            } else {
                None
            }
        })
    };
    result.push(part);
    if let Some(suffix) = suffix {
        result.push(suffix);
    }
}

#[cfg(test)]
mod tests {
    use super::{blocked, parse_console_command, result};

    #[test]
    fn public_console_parser_preserves_trimmed_command_and_legacy_error_reasons() {
        assert_eq!(parse_console_command("  at+CSQ ").unwrap(), "at+CSQ");
        for (input, expected) in [
            ("X", "命令必须以 AT 开头"),
            (
                "AT\n+CSQ",
                "AT 命令必须为单行，不能含换行、制表符或控制字符",
            ),
            (
                "AT\u{200b}",
                "AT 命令必须为单行，不能含换行、制表符或控制字符",
            ),
        ] {
            assert_eq!(parse_console_command(input).unwrap_err(), expected);
        }
        assert_eq!(
            parse_console_command("AT+NAME=\"unfinished").unwrap_err(),
            "AT 命令的引号未闭合"
        );
        assert_eq!(
            parse_console_command(&format!("AT{}", "X".repeat(4095))).unwrap_err(),
            "AT 命令不能超过 4096 字节"
        );
    }

    #[test]
    fn public_console_response_classification_matches_go_terminal_rules() {
        for (response, outcome, kind) in [
            ("AT+CSQ\r\nOK\r\n", "success", None),
            ("AT+CSQ\r\nERROR\r\n", "module_error", Some("generic")),
            ("+CME ERROR: 10\r\n", "module_error", Some("cme")),
            ("+CMS ERROR: 500\r\n", "module_error", Some("cms")),
            ("ERROR\r\n+CSQ: 10,99\r\n", "success", None),
        ] {
            let value = result(response);
            assert_eq!(value["response"], response);
            assert_eq!(value["outcome"], outcome);
            assert_eq!(
                value.get("error_kind").and_then(|value| value.as_str()),
                kind
            );
        }
    }

    #[test]
    fn cellular_probes_remain_blocked_under_flight_mode_policy() {
        assert!(blocked("ATE0;+COPS=?").unwrap());
        assert!(!blocked(r#"AT+NAME="AT+COPS=?""#).unwrap());
    }
}
