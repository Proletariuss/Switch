const SERVING_OPERATOR_BY_PLMN: &[(&str, &str)] = &[
    ("46000", "中国移动"),
    ("46002", "中国移动"),
    ("46004", "中国移动"),
    ("46007", "中国移动"),
    ("46008", "中国移动"),
    ("46013", "中国移动"),
    ("46001", "中国联通"),
    ("46006", "中国联通"),
    ("46009", "中国联通"),
    ("46003", "中国电信"),
    ("46005", "中国电信"),
    ("46011", "中国电信"),
    ("46015", "中国广电"),
    ("45400", "CSL"),
    ("45402", "CSL"),
    ("45410", "CSL"),
    ("45418", "CSL"),
    ("45403", "3 HK"),
    ("45416", "电讯盈科"),
    ("45419", "电讯盈科"),
    ("45404", "3 HK"),
    ("45406", "数码通"),
    ("45415", "数码通"),
    ("45407", "中国移动香港"),
    ("45412", "中国联通香港"),
    ("46601", "远传电信"),
    ("46602", "远传电信"),
    ("46605", "亚太电信"),
    ("46689", "台湾之星"),
    ("46692", "中华电信"),
    ("46693", "台湾大哥大"),
    ("46697", "台湾大哥大"),
    ("46699", "台湾大哥大"),
];

pub(crate) fn resolve_serving_operator_name(plmn: &str) -> String {
    let normalized = plmn.trim().trim_matches('"');
    if normalized.is_empty() {
        return String::new();
    }
    SERVING_OPERATOR_BY_PLMN
        .iter()
        .find_map(|(code, name)| (*code == normalized).then_some(*name))
        .unwrap_or(normalized)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::resolve_serving_operator_name;

    #[test]
    fn serving_operator_projection_matches_legacy_plmn_names_and_fallbacks() {
        for (input, expected) in [
            ("46000", "中国移动"),
            ("46001", "中国联通"),
            ("46011", "中国电信"),
            ("46015", "中国广电"),
            ("45400", "CSL"),
            ("45403", "3 HK"),
            ("45404", "3 HK"),
            ("45406", "数码通"),
            ("45407", "中国移动香港"),
            ("45412", "中国联通香港"),
            ("46601", "远传电信"),
            ("46605", "亚太电信"),
            ("46689", "台湾之星"),
            ("46692", "中华电信"),
            ("46693", "台湾大哥大"),
            ("  \"46015\"  ", "中国广电"),
            ("CHN-UNICOM", "CHN-UNICOM"),
            ("99999", "99999"),
            ("", ""),
        ] {
            assert_eq!(resolve_serving_operator_name(input), expected, "{input:?}");
        }
    }
}
