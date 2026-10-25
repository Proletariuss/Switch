//! Rust-owned eSIM note formats for the module's ME phonebook.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};

const RECORD_PREFIX: &str = "VH1";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Note {
    #[serde(default)]
    pub(crate) index: usize,
    #[serde(default)]
    pub(crate) iccid: String,
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) phone: String,
    #[serde(default)]
    pub(crate) tags: String,
}

static PHONEBOOK_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)\+CPBR:\s*(\d+),"([^"]*)",\d+,"([^"]*)""#).unwrap());
static PHONEBOOK_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\+CPBS:\s*"ME",(\d+),(\d+)"#).unwrap());

pub(crate) fn parse_phonebook_status(response: &str) -> Result<(usize, usize), &'static str> {
    let captures = PHONEBOOK_STATUS
        .captures(response)
        .ok_or("ME 通讯录容量未返回")?;
    let used = captures
        .get(1)
        .and_then(|value| value.as_str().parse().ok())
        .ok_or("ME 通讯录容量未返回")?;
    let total = captures
        .get(2)
        .and_then(|value| value.as_str().parse().ok())
        .ok_or("ME 通讯录容量未返回")?;
    Ok((used, total))
}

pub(crate) fn decode_phonebook(response: &str) -> (BTreeMap<String, Note>, Vec<usize>) {
    let mut notes = BTreeMap::new();
    let mut occupied = BTreeSet::new();
    for captures in PHONEBOOK_ENTRY.captures_iter(response) {
        let Some(index) = captures
            .get(1)
            .and_then(|value| value.as_str().parse().ok())
        else {
            continue;
        };
        let Some(text) = captures.get(3).map(|value| value.as_str()) else {
            continue;
        };
        occupied.insert(index);
        if let Some(note) = decode_note(index, text) {
            notes.insert(note.iccid.clone(), note);
        }
    }
    (notes, occupied.into_iter().collect())
}

fn decode_note(index: usize, text: &str) -> Option<Note> {
    let mut parts = text.split('|');
    if parts.next()? != RECORD_PREFIX {
        return None;
    }
    let iccid = parts.next()?;
    if iccid.trim().is_empty() {
        return None;
    }
    let decode = |value: &str| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    let label = decode(parts.next()?)?;
    let phone = decode(parts.next()?)?;
    let tags = decode(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(Note {
        index,
        iccid: iccid.to_owned(),
        label,
        phone,
        tags,
    })
}

pub(crate) fn encode_note(mut note: Note) -> Result<String, &'static str> {
    note.iccid = note.iccid.trim().to_owned();
    note.label = note.label.trim().to_owned();
    note.phone = note.phone.trim().to_owned();
    note.tags = note.tags.trim().to_owned();
    if note.iccid.is_empty() {
        return Err("iccid is required");
    }
    if !note
        .iccid
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err("iccid is invalid");
    }
    if note.label.chars().count() > 48
        || note.phone.chars().count() > 40
        || note.tags.chars().count() > 48
    {
        return Err("模块资料名称、手机号或标签过长");
    }
    let encode =
        |value: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes());
    let record = format!(
        "{RECORD_PREFIX}|{}|{}|{}|{}",
        note.iccid,
        encode(&note.label),
        encode(&note.phone),
        encode(&note.tags)
    );
    if record.len() > 255 {
        return Err("模块通讯录记录超过容量");
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::{Note, decode_note, decode_phonebook, encode_note, parse_phonebook_status};

    #[test]
    fn unicode_note_encoding_matches_the_module_phonebook_format() {
        let note = Note {
            index: 0,
            iccid: " 8985201234567890123 ".into(),
            label: "  香港🛰️  ".into(),
            phone: " +852 5123 4567 ".into(),
            tags: " 工作 ".into(),
        };
        assert_eq!(
            encode_note(note).unwrap(),
            "VH1|8985201234567890123|6aaZ5riv8J-bsO-4jw|Kzg1MiA1MTIzIDQ1Njc|5bel5L2c"
        );
    }

    #[test]
    fn unicode_limits_are_counted_in_characters_and_trimmed() {
        let mut note = Note {
            iccid: "8985".into(),
            label: "界".repeat(48),
            ..Note::default()
        };
        assert!(encode_note(note.clone()).is_ok());
        note.label.push('界');
        assert_eq!(encode_note(note), Err("模块资料名称、手机号或标签过长"));
    }

    #[test]
    fn decoder_rejects_malformed_records_and_preserves_exact_iccid() {
        let record = "VH1| 8985 |5rG96aaZ|Kzg1Mg|5bel5Lia";
        assert_eq!(decode_note(7, record).unwrap().iccid, " 8985 ");
        for invalid in [
            "VH1||5rG96aaZ|Kzg1Mg|5bel5Lia",
            "VH1|8985|not base64!|Kzg1Mg|5bel5Lia",
            "VH1|8985|5rG96aaZ|Kzg1Mg|5bel5Lia|extra",
            "VH0|8985|5rG96aaZ|Kzg1Mg|5bel5Lia",
        ] {
            assert!(decode_note(1, invalid).is_none(), "accepted {invalid}");
        }
    }

    #[test]
    fn decoder_replaces_invalid_utf8_like_go_json_encoding() {
        let note = decode_note(1, "VH1|8985|_w||").unwrap();
        assert_eq!(note.label, "\u{fffd}");
        assert_eq!(note.phone, "");
        assert_eq!(note.tags, "");
    }

    #[test]
    fn phonebook_parser_tracks_occupied_slots_and_last_duplicate_note() {
        let response = concat!(
            "+CPBR: 1,\"\",129,\"occupied\"\r\n",
            "+CPBR: 2,\"\",129,\"VH1|8985|QQ|MQ|\"\r\n",
            "+CPBR: 4,\"\",129,\"VH1|8985|Qg|Mg|\"\r\n",
            "+CPBR: 5,\"\",129,\"VH1|8986|!|MQ|\"\r\n"
        );
        let (notes, occupied) = decode_phonebook(response);
        assert_eq!(occupied, vec![1, 2, 4, 5]);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes["8985"].index, 4);
        assert_eq!(notes["8985"].label, "B");
    }

    #[test]
    fn record_limit_is_enforced_after_base64_encoding() {
        let note = Note {
            iccid: "8".repeat(230),
            label: "x".repeat(48),
            phone: "y".repeat(40),
            tags: "z".repeat(48),
            ..Note::default()
        };
        assert_eq!(encode_note(note), Err("模块通讯录记录超过容量"));
    }

    #[test]
    fn phonebook_status_parsing_preserves_used_and_total() {
        assert_eq!(
            parse_phonebook_status("+CPBS: \"ME\",7,250\r\nOK"),
            Ok((7, 250))
        );
        assert_eq!(parse_phonebook_status("OK"), Err("ME 通讯录容量未返回"));
    }

    #[test]
    fn command_injection_cannot_enter_the_phonebook_record() {
        let note = Note {
            iccid: "8985\",\r\nAT+CFUN=1".into(),
            label: "name".into(),
            ..Note::default()
        };
        assert_eq!(encode_note(note), Err("iccid is invalid"));
    }
}
