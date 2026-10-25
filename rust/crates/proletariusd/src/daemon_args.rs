//! The four historical string flags, parsed once before native owners start.
//! Values may themselves begin with `-`; only an argument in flag position is
//! interpreted as an option. Paths retain their native Unix bytes.
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct DaemonArgs {
    pub(crate) port: Option<OsString>,
    pub(crate) listen: Option<OsString>,
    pub(crate) backend: Option<OsString>,
    pub(crate) vowifi_config: Option<OsString>,
}

pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<DaemonArgs, String> {
    let mut result = DaemonArgs::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let bytes = argument.as_bytes();
        if bytes == b"--" || bytes == b"-" || !bytes.starts_with(b"-") {
            break;
        }
        let flag = bytes.strip_prefix(b"--").unwrap_or(&bytes[1..]);
        let (name, inline) = match flag.iter().position(|byte| *byte == b'=') {
            Some(index) => (&flag[..index], Some(&flag[index + 1..])),
            None => (flag, None),
        };
        let (slot, known_name) = match name {
            b"port" => (&mut result.port, "port"),
            b"listen" => (&mut result.listen, "listen"),
            b"backend" => (&mut result.backend, "backend"),
            b"vowifi-config" => (&mut result.vowifi_config, "vowifi-config"),
            // Do not echo unknown argument bytes: they may contain secrets.
            _ => return Err("unknown daemon option".to_owned()),
        };
        *slot = Some(match inline {
            Some(value) => OsString::from_vec(value.to_vec()),
            None => arguments
                .next()
                .ok_or_else(|| format!("flag needs an argument: -{known_name}"))?,
        });
    }
    Ok(result)
}

/// Go strings.TrimSpace semantics for path selection, without lossy conversion
/// of a Unix path containing invalid UTF-8. Invalid bytes are not whitespace.
pub(crate) fn trimmed(value: &OsStr) -> &OsStr {
    let bytes = value.as_bytes();
    let leading = bytes.utf8_chunks().next().map_or(0, |chunk| {
        chunk.valid().len() - chunk.valid().trim_start().len()
    });
    let trailing = bytes.utf8_chunks().last().map_or(0, |chunk| {
        if chunk.invalid().is_empty() {
            chunk.valid().len() - chunk.valid().trim_end().len()
        } else {
            0
        }
    });
    OsStr::from_bytes(&bytes[leading..bytes.len().saturating_sub(trailing).max(leading)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Result<DaemonArgs, String> {
        parse(values.iter().map(OsString::from))
    }

    #[test]
    fn four_flags_share_go_spelling_last_value_and_string_value_rules() {
        let parsed = args(&[
            "-port=old",
            "--port",
            "new",
            "--listen=unix:/old",
            "-listen",
            "unix:/new",
            "-backend=at",
            "--backend=qmi",
            "-vowifi-config=old",
            "--vowifi-config",
            "-backend=at",
        ])
        .unwrap();
        assert_eq!(parsed.port.as_deref(), Some(OsStr::new("new")));
        assert_eq!(parsed.listen.as_deref(), Some(OsStr::new("unix:/new")));
        assert_eq!(parsed.backend.as_deref(), Some(OsStr::new("qmi")));
        assert_eq!(
            parsed.vowifi_config.as_deref(),
            Some(OsStr::new("-backend=at"))
        );
        assert_eq!(
            args(&["-vowifi-config=a=b"]).unwrap().vowifi_config,
            Some(OsString::from("a=b"))
        );
        assert_eq!(
            args(&["-port", "--", "-backend=qmi"]).unwrap().port,
            Some(OsString::from("--"))
        );
    }

    #[test]
    fn terminators_and_positionals_stop_all_flag_parsing() {
        for stop in ["--", "-", "positional", ""] {
            let parsed = args(&["-backend=at", stop, "-backend=qmi", "-unknown"]).unwrap();
            assert_eq!(parsed.backend, Some(OsString::from("at")));
        }
    }

    #[test]
    fn missing_or_unknown_options_fail_without_echoing_values() {
        for flag in ["-port", "--listen", "-backend", "--vowifi-config"] {
            assert!(args(&[flag]).is_err());
        }
        for flag in ["-secret-token=value", "---port=path", "-=value"] {
            assert_eq!(args(&[flag]).unwrap_err(), "unknown daemon option");
        }
        assert_eq!(
            args(&["-vowifi-config="]).unwrap().vowifi_config,
            Some(OsString::new())
        );
    }

    #[test]
    fn native_path_bytes_and_unicode_whitespace_survive_selection() {
        let value = OsString::from_vec(b" \xff/a ".to_vec());
        let parsed = parse([OsString::from("-vowifi-config"), value.clone()]).unwrap();
        assert_eq!(parsed.vowifi_config, Some(value.clone()));
        assert_eq!(trimmed(&value).as_bytes(), b"\xff/a");
        assert_eq!(trimmed(OsStr::new("\u{3000}\tpath\u{85}\n")), "path");
        assert!(trimmed(OsStr::new(" \u{2000}\n")).is_empty());
    }
}
