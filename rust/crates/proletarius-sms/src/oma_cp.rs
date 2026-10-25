//! Bounded WBXML parsing and text projection for WAP OMA Client Provisioning.
use std::collections::BTreeMap;

mod tokens {
    include!("oma_cp_tokens.rs");
}

#[derive(Debug, Default)]
struct Characteristic {
    kind: String,
    params: BTreeMap<String, String>,
    children: Vec<Characteristic>,
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
    string_table: &'a [u8],
    tag_page: u8,
    attr_page: u8,
}

impl Reader<'_> {
    fn read_byte(&mut self) -> Result<u8, ()> {
        let value = *self.data.get(self.position).ok_or(())?;
        self.position += 1;
        Ok(value)
    }

    fn read_mb_uint32(&mut self) -> Result<u32, ()> {
        let mut value = 0_u32;
        for _ in 0..5 {
            let byte = self.read_byte()?;
            value = value.wrapping_shl(7) | u32::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(())
    }

    fn read_inline(&mut self) -> String {
        let start = self.position;
        while self.position < self.data.len() && self.data[self.position] != 0 {
            self.position += 1;
        }
        let text = String::from_utf8_lossy(&self.data[start..self.position]).into_owned();
        if self.position < self.data.len() {
            self.position += 1;
        }
        text
    }

    fn string_at(&self, offset: u32) -> String {
        let Ok(start) = usize::try_from(offset) else {
            return String::new();
        };
        if start >= self.string_table.len() {
            return String::new();
        }
        let tail = &self.string_table[start..];
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(tail.len());
        String::from_utf8_lossy(&tail[..end]).into_owned()
    }

    fn read_opaque(&mut self) -> Result<(), ()> {
        let length = usize::try_from(self.read_mb_uint32()?).map_err(|_| ())?;
        self.position = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or(())?;
        Ok(())
    }

    fn parse_attributes(&mut self) -> BTreeMap<String, String> {
        let mut attributes = BTreeMap::new();
        let mut current_name = String::new();
        let mut current_value = String::new();
        while let Ok(token) = self.read_byte() {
            match token {
                0x01 => {
                    self.flush_attribute(&mut attributes, &mut current_name, &mut current_value);
                    break;
                }
                0x00 => {
                    if let Ok(page) = self.read_byte() {
                        self.attr_page = page;
                    } else {
                        break;
                    }
                }
                0x03 => current_value.push_str(&self.read_inline()),
                0x83 => match self.read_mb_uint32() {
                    Ok(offset) => current_value.push_str(&self.string_at(offset)),
                    Err(()) => break,
                },
                0xc3 => {
                    if self.read_opaque().is_err() {
                        break;
                    }
                }
                _ => {
                    if !current_name.is_empty()
                        && let Some(value) = tokens::attr_value(self.attr_page, token)
                    {
                        current_value.push_str(value);
                        continue;
                    }
                    if let Some((name, prefix)) = tokens::attr_start(self.attr_page, token) {
                        self.flush_attribute(
                            &mut attributes,
                            &mut current_name,
                            &mut current_value,
                        );
                        current_name.push_str(name);
                        current_value.push_str(prefix);
                    } else if let Some(value) = tokens::attr_value(self.attr_page, token) {
                        current_value.push_str(value);
                    }
                }
            }
        }
        self.flush_attribute(&mut attributes, &mut current_name, &mut current_value);
        attributes
    }

    fn flush_attribute(
        &self,
        attributes: &mut BTreeMap<String, String>,
        name: &mut String,
        value: &mut String,
    ) {
        if !name.is_empty() {
            attributes.insert(name.clone(), value.trim().to_string());
        }
        name.clear();
        value.clear();
    }

    fn parse_element(&mut self, depth: usize) -> Result<Option<Characteristic>, ()> {
        if depth > 128 {
            return Err(());
        }
        let token = self.read_byte()?;
        if token == 0x00 {
            self.tag_page = self.read_byte()?;
            return self.parse_element(depth + 1);
        }
        if token == 0x01 {
            return Err(());
        }
        let has_content = token & 0x40 != 0;
        let has_attributes = token & 0x80 != 0;
        let tag = tokens::tag_name(self.tag_page, token & 0x3f)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("tag_0x{:02X}", token & 0x3f));
        let attributes = if has_attributes {
            self.parse_attributes()
        } else {
            BTreeMap::new()
        };

        let mut children = Vec::new();
        let mut text = String::new();
        if has_content {
            while self.position < self.data.len() {
                match self.data[self.position] {
                    0x01 => {
                        self.position += 1;
                        break;
                    }
                    0x03 => {
                        self.position += 1;
                        text.push_str(&self.read_inline());
                    }
                    0x83 => {
                        self.position += 1;
                        match self.read_mb_uint32() {
                            Ok(offset) => text.push_str(&self.string_at(offset)),
                            Err(()) => break,
                        }
                    }
                    0xc3 => {
                        self.position += 1;
                        if self.read_opaque().is_err() {
                            break;
                        }
                    }
                    0x00 => {
                        self.position += 1;
                        match self.read_byte() {
                            Ok(page) => self.tag_page = page,
                            Err(()) => break,
                        }
                    }
                    _ => match self.parse_element(depth + 1) {
                        Ok(Some(child)) => children.push(child),
                        Ok(None) => {}
                        Err(()) => break,
                    },
                }
            }
        }

        match tag.as_str() {
            "characteristic" => {
                let mut node = Characteristic {
                    kind: attributes.get("type").cloned().unwrap_or_default(),
                    params: BTreeMap::new(),
                    children: Vec::new(),
                };
                for child in children {
                    if child.kind == "__parm__" {
                        node.params.extend(child.params);
                    } else {
                        node.children.push(child);
                    }
                }
                Ok(Some(node))
            }
            "parm" => {
                let name = attributes
                    .get("name")
                    .filter(|name| !name.trim().is_empty())
                    .cloned()
                    .or_else(|| (!text.is_empty()).then_some(text));
                let Some(name) = name else {
                    return Ok(None);
                };
                let mut params = BTreeMap::new();
                params.insert(name, attributes.get("value").cloned().unwrap_or_default());
                Ok(Some(Characteristic {
                    kind: "__parm__".into(),
                    params,
                    children: Vec::new(),
                }))
            }
            "wap-provisioningdoc" if !children.is_empty() => Ok(Some(Characteristic {
                kind: tag,
                params: BTreeMap::new(),
                children,
            })),
            _ => Ok(None),
        }
    }
}

fn read_mb_uint32(data: &[u8], position: &mut usize) -> Option<u32> {
    let mut value = 0_u32;
    for _ in 0..5 {
        let byte = *data.get(*position)?;
        *position += 1;
        value = value.wrapping_shl(7) | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn is_wbxml_header(data: &[u8]) -> bool {
    if data.len() < 4 || !(1..=3).contains(&data[0]) {
        return false;
    }
    let mut position = 1;
    let Some(public_id) = read_mb_uint32(data, &mut position) else {
        return false;
    };
    if public_id != 0x0b || read_mb_uint32(data, &mut position).is_none() {
        return false;
    }
    let Some(string_table_length) = read_mb_uint32(data, &mut position) else {
        return false;
    };
    usize::try_from(string_table_length)
        .ok()
        .and_then(|length| position.checked_add(length))
        .is_some_and(|end| end <= data.len())
}

fn find_wbxml_start(data: &[u8]) -> Option<&[u8]> {
    if is_wbxml_header(data) {
        return Some(data);
    }
    (1..data.len())
        .map(|offset| &data[offset..])
        .find(|tail| is_wbxml_header(tail))
}

pub(super) fn decode(data: &[u8]) -> Result<String, ()> {
    if data.len() < 4 {
        return Err(());
    }
    let document = find_wbxml_start(data).ok_or(())?;
    let version = *document.first().ok_or(())?;
    let mut position = 1;
    if read_mb_uint32(document, &mut position) != Some(0x0b) {
        return Err(());
    }
    read_mb_uint32(document, &mut position).ok_or(())?;
    let table_length =
        usize::try_from(read_mb_uint32(document, &mut position).ok_or(())?).map_err(|_| ())?;
    let table_end = position
        .checked_add(table_length)
        .filter(|end| *end <= document.len())
        .ok_or(())?;
    let string_table = &document[position..table_end];
    let mut reader = Reader {
        data: document,
        position: table_end,
        string_table,
        tag_page: 0,
        attr_page: 0,
    };
    let mut roots = Vec::new();
    while reader.position < reader.data.len() {
        match reader.parse_element(0) {
            Ok(Some(root)) => roots.push(root),
            Ok(None) => {}
            Err(()) => break,
        }
    }
    if roots.is_empty() {
        return Err(());
    }
    let mut output = String::new();
    for (index, root) in roots.iter().enumerate() {
        if index != 0 {
            output.push('\n');
        }
        format_characteristic(&mut output, root, 0);
    }
    if output.is_empty() {
        return Err(());
    }
    let _version = version;
    Ok(output.trim().to_string())
}

fn format_characteristic(output: &mut String, value: &Characteristic, indent: usize) {
    if value.kind == "wap-provisioningdoc" {
        for (index, child) in value.children.iter().enumerate() {
            if index != 0 {
                output.push('\n');
            }
            format_characteristic(output, child, indent);
        }
        return;
    }
    let prefix = "  ".repeat(indent);
    output.push_str(&prefix);
    output.push_str("📋 ");
    output.push_str(&value.kind);
    if value.kind == "APPLICATION"
        && let Some(app_id) = value
            .params
            .get("APPID")
            .and_then(|id| tokens::app_id_name(id))
    {
        output.push_str(" (");
        output.push_str(app_id);
        output.push(')');
    }
    output.push('\n');
    for (name, parameter) in &value.params {
        output.push_str(&prefix);
        output.push_str("  ");
        output.push_str(name);
        output.push_str(": ");
        output.push_str(parameter);
        output.push('\n');
    }
    for child in &value.children {
        format_characteristic(output, child, indent + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        include_bytes!("../testdata/oma-cp/provisioning.wbxml").to_vec()
    }

    fn fixture_root(data: &[u8]) -> Characteristic {
        assert_eq!(&data[..4], &[0x03, 0x0b, 0x6a, 0]);
        let mut reader = Reader {
            data,
            position: 4,
            string_table: &[],
            tag_page: 0,
            attr_page: 0,
        };
        let root = reader.parse_element(0).unwrap().unwrap();
        assert_eq!(reader.position, data.len());
        assert_eq!(root.kind, "wap-provisioningdoc");
        root
    }

    #[test]
    fn historical_go_header_matrix_and_multibyte_public_id() {
        for (name, data, expected) in [
            ("valid", &[0x03, 0x0b, 0x6a, 0x00][..], true),
            ("short", &[0x03][..], false),
            ("version", &[0x04, 0x0b, 0x6a, 0x00][..], false),
            ("public ID", &[0x03, 0x0c, 0x6a, 0x00][..], false),
            ("zero", &[0x00, 0x00, 0x00, 0x00][..], false),
            (
                "multibyte public ID",
                &[0x03, 0x80, 0x0b, 0x6a, 0x00][..],
                true,
            ),
        ] {
            assert_eq!(is_wbxml_header(data), expected, "{name}");
        }
    }

    #[test]
    fn historical_go_provisioning_tree_semantics() {
        let root = fixture_root(&fixture());
        assert_eq!(root.children.len(), 2);
        let napdef = &root.children[0];
        assert_eq!(napdef.kind, "NAPDEF");
        for (key, expected) in [
            ("NAPID", "internet"),
            ("NAP-ADDRESS", "internet.telekom"),
            ("BEARER", "GSM-GPRS"),
            ("NAP-ADDRTYPE", "APN"),
        ] {
            assert_eq!(napdef.params.get(key).map(String::as_str), Some(expected));
        }
        let app = &root.children[1];
        assert_eq!(app.kind, "APPLICATION");
        assert_eq!(app.params.get("APPID").map(String::as_str), Some("w4"));
        assert_eq!(app.params.get("NAME").map(String::as_str), Some("Telekom"));
    }

    #[test]
    fn historical_go_attribute_value_ambiguity_and_orphan_parameter() {
        let data = include_bytes!("../testdata/oma-cp/attribute-value-orphan.wbxml");
        let root = fixture_root(data);
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.kind, "NAPDEF");
        assert_eq!(child.params.get("NAME").map(String::as_str), Some("NAPDEF"));
        assert!(!child.params.contains_key(""));
        assert_eq!(child.params.len(), 1);
        let summary = decode(data).unwrap();
        assert!(summary.contains("NAME: NAPDEF"));
        assert!(!summary.contains("orphan"));
    }

    #[test]
    fn historical_go_short_and_long_wsp_and_full_encrypted_vectors() {
        for data in [
            &include_bytes!("../testdata/oma-cp/short-wsp.bin")[..],
            &include_bytes!("../testdata/oma-cp/long-wsp.bin")[..],
        ] {
            let summary = decode(data).unwrap();
            assert!(summary.contains("NAP-ADDRESS: internet.telekom"));
            assert!(summary.contains("NAME: Telekom"));
        }
        assert!(decode(include_bytes!("../testdata/oma-cp/encrypted.bin")).is_err());
    }

    #[test]
    fn decodes_the_oma_provisioning_wbxml_fixture() {
        let summary = decode(&fixture()).unwrap();
        for expected in [
            "NAPDEF",
            "NAPID: internet",
            "NAP-ADDRESS: internet.telekom",
            "BEARER: GSM-GPRS",
            "NAP-ADDRTYPE: APN",
            "APPLICATION (浏览器书签)",
            "NAME: Telekom",
        ] {
            assert!(
                summary.contains(expected),
                "missing {expected:?}: {summary}"
            );
        }
    }

    #[test]
    fn finds_wbxml_after_a_long_wsp_header_and_rejects_encrypted_payload() {
        let mut wrapped = vec![1; 24];
        wrapped.extend(fixture());
        assert!(decode(&wrapped).is_ok());
        assert!(decode(&[0, 0x48, 0x15, 0x0e, 0x22, 0x15, 0x15, 0xb0]).is_err());
    }
}
