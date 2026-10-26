//! Public method compatibility, independent of the Go fallback lifetime.
//!
//! This reviewed table is checked against independently extracted dispatch and
//! ownership declarations by the migration contract gate. It is not a build-time
//! or runtime Go dependency. Only an explicitly declared public GET may alias HEAD.
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Body;
use hyper::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING};
use hyper::{Method, Response, StatusCode, Uri};

const ROUTES: &[(&str, &str)] = &[
    ("/api/at", "POST"),
    ("/api/call-mode/backups", "GET, HEAD"),
    ("/api/call-mode/backups/delete", "POST"),
    ("/api/call-mode/backups/export", "GET, HEAD"),
    ("/api/call-mode/backups/import", "POST"),
    ("/api/call-mode/status", "GET, HEAD"),
    ("/api/call-mode/download", "POST"),
    ("/api/call-mode/enable", "POST"),
    ("/api/call-mode/restore", "POST"),
    ("/api/call-mode/retry", "POST"),
    ("/api/vowifi", "GET, HEAD, POST"),
    ("/api/vowifi/reconcile", "POST"),
    ("/api/vowifi/stop-control", "GET, HEAD, POST"),
    ("/api/reader/vowifi/ussd", "POST"),
    ("/api/vowifi/calls/dial", "POST"),
    ("/api/vowifi/calls/answer", "POST"),
    ("/api/vowifi/calls/reject", "POST"),
    ("/api/vowifi/calls/hangup", "POST"),
    ("/api/vowifi/calls/dtmf", "POST"),
    ("/api/reader/vowifi/calls/dial", "POST"),
    ("/api/reader/vowifi/calls/answer", "POST"),
    ("/api/reader/vowifi/calls/reject", "POST"),
    ("/api/reader/vowifi/calls/hangup", "POST"),
    ("/api/reader/vowifi/calls/dtmf", "POST"),
    ("/api/vowifi/result", "GET, HEAD"),
    ("/api/reader/vowifi", "GET, HEAD, POST"),
    ("/api/reader/vowifi/reconcile", "POST"),
    ("/api/reader/vowifi/stop-control", "GET, HEAD, POST"),
    ("/api/reader/vowifi/sessions", "GET, HEAD"),
    ("/api/reader/vowifi/calls/status", "GET, HEAD"),
    ("/api/vowifi/calls/status", "GET, HEAD"),
    ("/api/call/answer", "POST"),
    ("/api/call/audio/start", "POST"),
    ("/api/call/audio/stop", "POST"),
    ("/api/call/dial", "POST"),
    ("/api/call/dtmf", "POST"),
    ("/api/call/hangup", "POST"),
    ("/api/call/status", "GET, HEAD"),
    ("/api/calls", "GET, HEAD"),
    ("/api/calls/clear", "POST"),
    ("/api/calls/delete", "POST"),
    ("/api/diagnostics/events", "GET, HEAD"),
    ("/api/diagnostics/support", "GET, HEAD"),
    ("/api/esim/notes", "GET, HEAD, PUT"),
    ("/api/esim/module-notes", "GET, HEAD, PUT"),
    ("/api/esim", "GET, HEAD"),
    ("/api/esim/mutation-intent", "GET, HEAD"),
    ("/api/esim/download", "POST"),
    ("/api/esim/download/confirm", "POST"),
    ("/api/esim/health", "GET, HEAD"),
    ("/api/esim/phonebook/probe", "POST"),
    ("/api/esim/profile", "DELETE, PATCH"),
    ("/api/esim/switch", "POST"),
    ("/api/health", "GET, HEAD"),
    ("/api/status", "GET, HEAD"),
    ("/api/module/flight-mode", "GET, HEAD, POST"),
    ("/api/multistandby/changes", "GET, HEAD"),
    ("/api/multistandby/conversations", "GET, HEAD"),
    ("/api/multistandby/cards", "GET, HEAD"),
    ("/api/multistandby/control-intents", "POST"),
    ("/api/multistandby/control-intents/ack", "POST"),
    ("/api/multistandby/history", "GET, HEAD"),
    ("/api/multistandby/messages", "GET, HEAD"),
    ("/api/multistandby/messages/send", "POST"),
    ("/api/multistandby/messages/{message_id}", "DELETE"),
    ("/api/multistandby/operations/{id}", "GET, HEAD"),
    ("/api/multistandby/profiles", "GET, HEAD"),
    ("/api/multistandby/report", "GET, HEAD"),
    ("/api/multistandby/retry", "POST"),
    ("/api/multistandby/start", "POST"),
    ("/api/multistandby/stop", "POST"),
    ("/api/multistandby/send-intents", "GET, HEAD, POST"),
    ("/api/multistandby/send-intents/submit", "POST"),
    ("/api/network/check-4g", "POST"),
    ("/api/network", "GET, HEAD"),
    ("/api/network/reboot-module", "POST"),
    ("/api/network/recovery", "GET, HEAD, POST"),
    ("/api/network/services", "GET, HEAD"),
    ("/api/network/services-order", "PUT"),
    ("/api/network/traffic", "GET, HEAD"),
    ("/api/network/usbnet", "POST"),
    ("/api/reader/ble/status", "GET, HEAD"),
    ("/api/reader/ble/scan", "POST"),
    ("/api/reader/ble/connect", "POST"),
    ("/api/reader/ble/disconnect", "POST"),
    ("/api/reader/auth", "POST"),
    ("/api/reader/connect", "POST"),
    ("/api/reader/disconnect", "POST"),
    ("/api/reader/refresh", "POST"),
    ("/api/reader/esim", "GET, HEAD"),
    ("/api/reader/esim/mutation-intent", "GET, HEAD"),
    ("/api/reader/esim/health", "GET, HEAD"),
    ("/api/reader/esim/download", "POST"),
    ("/api/reader/esim/download/confirm", "POST"),
    ("/api/reader/esim/notifications", "GET, HEAD"),
    ("/api/reader/esim/notifications/retry", "POST"),
    ("/api/reader/esim/operation", "GET, HEAD"),
    ("/api/esim/operation", "GET, HEAD"),
    ("/api/esim/operation/acknowledge", "POST"),
    ("/api/reader/imei", "DELETE, GET, HEAD, PUT"),
    ("/api/reader/esim/profile", "DELETE, PATCH"),
    ("/api/reader/esim/switch", "POST"),
    ("/api/reader/info", "GET, HEAD"),
    ("/api/reader/list", "GET, HEAD"),
    ("/api/reader/vowifi/proxy-check", "POST"),
    ("/api/sms", "GET, HEAD"),
    ("/api/sms/devices", "GET, HEAD"),
    ("/api/sms/adopt", "GET, HEAD, POST"),
    ("/api/sms/clear", "POST"),
    ("/api/sms/delete", "POST"),
    ("/api/sms/delete-sender", "POST"),
    ("/api/sms/refresh", "POST"),
    ("/api/sms/storage", "GET, HEAD"),
    ("/api/sms/forwarding/configure", "POST"),
    ("/api/sms/forwarding/device-snapshot", "POST"),
    ("/api/sms/forwarding/enqueue", "POST"),
    ("/api/sms/forwarding/queue-action", "POST"),
    ("/api/sms/forwarding/status", "GET, HEAD"),
    ("/api/sms/forwarding/test", "POST"),
    ("/api/sms/send", "POST"),
    ("/api/sms/send-intents", "GET, HEAD, POST"),
    ("/api/sms/status", "GET, HEAD"),
    ("/api/v2/call/dtmf", "POST"),
    ("/api/v2/call/negotiate-sdp", "POST"),
    ("/api/v2/call/build-local-sdp", "POST"),
    ("/api/v2/call/invite-policy", "POST"),
    ("/api/v2/carrier/resolve", "POST"),
    ("/api/v2/device-snapshot", "GET, HEAD"),
    ("/api/v2/ims/validate-register-response", "POST"),
    ("/api/v2/migration-readiness", "GET, HEAD"),
    ("/api/v2/reader/authenticate", "POST"),
    ("/api/v2/reader/eap-aka", "POST"),
    ("/api/v2/reader/identity", "POST"),
    ("/api/v2/runtime", "GET, HEAD"),
    ("/api/v2/sms-archive", "GET, HEAD"),
    ("/api/v2/sms-archive/clear", "POST"),
    ("/api/v2/sms-archive/delete", "POST"),
    ("/api/v2/sms-archive/delete-sender", "POST"),
    ("/api/v2/sms-export", "POST"),
    ("/api/v2/sms-export/{token}", "DELETE, GET, HEAD"),
    ("/api/v2/sms/decode-incoming-tpdu", "POST"),
    ("/api/v2/sms/decode-rpdu", "POST"),
    ("/api/v2/sms/decode-stored-snapshot", "POST"),
    ("/api/v2/sms/encode-rpdu", "POST"),
    ("/api/v2/sms/forwarding/handoff", "GET, HEAD, POST"),
    ("/api/v2/sms/prepare-tpdu", "POST"),
    ("/api/v2/vowifi/eap-exchange/release", "POST"),
    ("/api/v2/vowifi/eap-identity", "POST"),
    ("/api/v2/vowifi/native-session-plan", "POST"),
    ("/api/v2/vowifi/proxy-policy", "POST"),
    ("/api/v2/vowifi/proxy-probe", "POST"),
    ("/api/v2/vowifi/select-transport", "POST"),
    ("/api/v2/vowifi/session-policy", "POST"),
    ("/api/voice/enable", "POST"),
    ("/api/voice/enabled", "GET, HEAD"),
];

// These paths previously used explicit Go ServeMux registrations. Other native
// endpoints retain their existing handler/fallback errors (notably sms-export's
// 400 invalid-request response); a method table is not authority to change them.
const LEGACY_METHOD_PATHS: &[&str] = &[
    "/api/vowifi",
    "/api/vowifi/result",
    "/api/reader/vowifi/ussd",
    "/api/vowifi/calls/dial",
    "/api/vowifi/calls/answer",
    "/api/vowifi/calls/reject",
    "/api/vowifi/calls/hangup",
    "/api/vowifi/calls/dtmf",
    "/api/reader/vowifi/calls/dial",
    "/api/reader/vowifi/calls/answer",
    "/api/reader/vowifi/calls/reject",
    "/api/reader/vowifi/calls/hangup",
    "/api/reader/vowifi/calls/dtmf",
    "/api/reader/vowifi",
    "/api/reader/vowifi/sessions",
    "/api/call/answer",
    "/api/call/audio/start",
    "/api/call/audio/stop",
    "/api/call/dial",
    "/api/call/dtmf",
    "/api/call/hangup",
    "/api/call/status",
    "/api/reader/vowifi/calls/status",
    "/api/vowifi/calls/status",
    "/api/call-mode/backups",
    "/api/call-mode/backups/delete",
    "/api/call-mode/backups/export",
    "/api/call-mode/backups/import",
    "/api/call-mode/status",
    "/api/call-mode/download",
    "/api/call-mode/enable",
    "/api/call-mode/restore",
    "/api/call-mode/retry",
    "/api/calls",
    "/api/calls/clear",
    "/api/calls/delete",
    "/api/health",
    "/api/status",
    "/api/multistandby/messages/send",
    "/api/diagnostics/events",
    "/api/esim/notes",
    "/api/esim/module-notes",
    "/api/esim/phonebook/probe",
    "/api/esim/switch",
    "/api/esim/profile",
    "/api/module/flight-mode",
    "/api/multistandby/cards",
    "/api/multistandby/operations/{id}",
    "/api/multistandby/report",
    "/api/multistandby/retry",
    "/api/multistandby/start",
    "/api/multistandby/stop",
    "/api/network/check-4g",
    "/api/network",
    "/api/network/reboot-module",
    "/api/network/services",
    "/api/network/services-order",
    "/api/network/traffic",
    "/api/network/usbnet",
    "/api/reader/ble/status",
    "/api/reader/ble/scan",
    "/api/reader/ble/connect",
    "/api/reader/ble/disconnect",
    "/api/reader/list",
    "/api/reader/info",
    "/api/reader/auth",
    "/api/reader/connect",
    "/api/reader/disconnect",
    "/api/reader/refresh",
    "/api/reader/vowifi/proxy-check",
    "/api/reader/imei",
    "/api/reader/esim",
    "/api/reader/esim/health",
    "/api/esim/health",
    "/api/esim",
    "/api/esim/download",
    "/api/esim/download/confirm",
    "/api/reader/esim/operation",
    "/api/reader/esim/notifications",
    "/api/reader/esim/notifications/retry",
    "/api/reader/esim/download",
    "/api/reader/esim/download/confirm",
    "/api/reader/esim/profile",
    "/api/reader/esim/switch",
    "/api/voice/enable",
    "/api/voice/enabled",
    "/api/sms",
    "/api/sms/adopt",
    "/api/sms/clear",
    "/api/sms/delete",
    "/api/sms/delete-sender",
    "/api/sms/send",
    "/api/sms/send-intents",
    "/api/sms/forwarding/status",
    "/api/sms/forwarding/configure",
    "/api/sms/forwarding/enqueue",
    "/api/sms/forwarding/test",
    "/api/sms/forwarding/queue-action",
    "/api/sms/status",
    "/api/sms/storage",
];

// Native additions define explicit method rejection without claiming membership
// in the independently frozen Go compatibility corpus.
const NATIVE_METHOD_PATHS: &[&str] = &["/api/diagnostics/support"];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Allowed,
    HeadAsGet,
    MethodNotAllowed(&'static str),
    Unknown,
}

fn legacy_method_path(path: &str) -> bool {
    LEGACY_METHOD_PATHS.iter().any(|pattern| {
        if let Some((prefix, _)) = pattern.split_once('{') {
            path.strip_prefix(prefix)
                .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
        } else {
            path == *pattern
        }
    })
}

/// Return only a reviewed static route template; never an actual dynamic ID.
pub(crate) fn diagnostic_route(path: &str) -> Option<&'static str> {
    ROUTES.iter().find_map(|(pattern, _)| {
        let matched = if let Some((prefix, _)) = pattern.split_once('{') {
            path.strip_prefix(prefix)
                .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
        } else {
            path == *pattern
        };
        matched.then_some(*pattern)
    })
}

fn allowed_methods(path: &str) -> Option<&'static str> {
    ROUTES.iter().find_map(|(pattern, methods)| {
        let matched = if let Some((prefix, _)) = pattern.split_once('{') {
            path.strip_prefix(prefix)
                .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
        } else {
            path == *pattern
        };
        matched.then_some(*methods)
    })
}

pub(crate) fn admission(method: &Method, path: &str) -> Admission {
    let Some(allowed) = allowed_methods(path) else {
        return Admission::Unknown;
    };
    if !allowed
        .split(", ")
        .any(|candidate| candidate == method.as_str())
    {
        return if legacy_method_path(path) || NATIVE_METHOD_PATHS.contains(&path) {
            Admission::MethodNotAllowed(allowed)
        } else {
            Admission::Unknown
        };
    }
    if method == Method::HEAD {
        Admission::HeadAsGet
    } else {
        Admission::Allowed
    }
}

pub(crate) fn head_uses_get(path: &str) -> bool {
    matches!(admission(&Method::HEAD, path), Admission::HeadAsGet)
}

/// Resolve only explicitly declared public patterns. Literal path segments
/// are compared after one percent decode, as in the frozen Go mux. A decoded
/// slash never becomes a separator, and wildcard data keeps its original raw
/// spelling. Unknown/private paths and paths needing a canonical redirect are
/// left untouched. This is not general URL.Path or wildcard-value decoding.
pub(crate) fn public_alias_uri(uri: &Uri) -> Option<Uri> {
    let raw = uri.path();
    if !raw.as_bytes().contains(&b'%') || clean_path(raw) != raw {
        return None;
    }
    let raw_segments = raw.split('/').collect::<Vec<_>>();
    let decoded = raw_segments
        .iter()
        .map(|segment| decode_alias_segment(segment))
        .collect::<Option<Vec<_>>>()?;
    let path = ROUTES.iter().find_map(|(pattern, _)| {
        let pattern_segments = pattern.split('/').collect::<Vec<_>>();
        if pattern_segments.len() != raw_segments.len() {
            return None;
        }
        let mut output = Vec::with_capacity(pattern_segments.len());
        for (index, literal) in pattern_segments.iter().enumerate() {
            if literal.starts_with('{') && literal.ends_with('}') {
                if raw_segments[index].is_empty() {
                    return None;
                }
                output.push(raw_segments[index]);
            } else {
                if decoded[index].as_slice() != literal.as_bytes() {
                    return None;
                }
                output.push(*literal);
            }
        }
        Some(output.join("/"))
    })?;
    if path == raw {
        return None;
    }
    let mut path_and_query = path;
    if let Some(query) = uri.query() {
        path_and_query.push('?');
        path_and_query.push_str(query);
    }
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    Uri::from_parts(parts).ok()
}

fn decode_alias_segment(segment: &str) -> Option<Vec<u8>> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex(*bytes.get(index + 1)?)?;
            let low = hex(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(decoded)
}

/// Go parses path escapes before it calls ServeMux. Keep malformed path
/// escapes out of auth, redirects and dispatch, while leaving RawQuery opaque.
pub(crate) fn invalid_path_response(uri: &Uri) -> Option<Response<Full<Bytes>>> {
    decode_alias_segment(uri.path()).is_none().then(|| {
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .header("connection", "close")
            .body(Full::new(Bytes::from_static(b"400 Bad Request")))
            .expect("fixed malformed-path response")
    })
}

/// Extract a single wildcard only after routing on the escaped segments. Go's
/// PathValue unescapes once; a decoded slash is parameter data, never routing.
pub(crate) fn path_value(raw: &str) -> Option<String> {
    String::from_utf8(decode_alias_segment(raw)?).ok()
}

fn escape_redirect_path(path: &str) -> String {
    // net/url.URL{Path: cleanedEscapedPath}.String(), encodePath mode.
    // In particular the existing percent signs must be escaped again.
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut escaped = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_'
                    | b'.'
                    | b'~'
                    | b'$'
                    | b'&'
                    | b'+'
                    | b','
                    | b'/'
                    | b':'
                    | b';'
                    | b'='
                    | b'@'
            )
        {
            escaped.push(char::from(byte));
        } else {
            escaped.push('%');
            escaped.push(char::from(HEX[(byte >> 4) as usize]));
            escaped.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    escaped
}

/// Go's current `http.ServeMux` redirects non-CONNECT requests containing
/// repeated slashes or literal `.` / `..` path segments to a cleaned escaped
/// path before dispatch. Keep percent-escaped segments opaque here: ServeMux
/// does not treat `%2e` or `%2f` as path separators during this cleaning step.
pub(crate) fn canonical_path_redirect(method: &Method, uri: &Uri) -> Option<Response<Full<Bytes>>> {
    if method == Method::CONNECT {
        return None;
    }
    let path = uri.path();
    let canonical = clean_path(path);
    if canonical == path {
        return None;
    }

    let mut location = escape_redirect_path(&canonical);
    // ServeMux constructs a fresh URL without ForceQuery on this branch.
    if let Some(query) = uri.query().filter(|query| !query.is_empty()) {
        location.push('?');
        location.push_str(query);
    }
    let mut builder = Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header("location", &location);
    let body = if method == Method::GET {
        builder = builder.header(CONTENT_TYPE, "text/html; charset=utf-8");
        Bytes::from(format!(
            "<a href=\"{}\">Temporary Redirect</a>.\n\n",
            html_escape(&location)
        ))
    } else {
        if method == Method::HEAD {
            builder = builder.header(CONTENT_TYPE, "text/html; charset=utf-8");
        }
        Bytes::new()
    };
    Some(
        builder
            .body(Full::new(body))
            .expect("canonical redirect response is valid"),
    )
}

fn clean_path(path: &str) -> String {
    let path = if path.is_empty() { "/" } else { path };
    let absolute;
    let path = if path.starts_with('/') {
        path
    } else {
        absolute = format!("/{path}");
        &absolute
    };
    let trailing_slash = path.ends_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let mut cleaned = format!("/{}", segments.join("/"));
    if trailing_slash && cleaned != "/" {
        cleaned.push('/');
    }
    cleaned
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

pub(crate) fn method_not_allowed(allowed: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(ALLOW, allowed)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("x-content-type-options", "nosniff")
        .header("x-frame-options", "DENY")
        .header("referrer-policy", "no-referrer")
        .body(Full::new(Bytes::from_static(b"Method Not Allowed\n")))
        .expect("fixed method response")
}

/// Drop only the wire body. A proxied HEAD may describe a nonempty representation
/// with an empty local body: its original Content-Length must survive unchanged.
pub(crate) fn without_head_body(
    mut response: Response<Full<Bytes>>,
    native_get: bool,
) -> Response<Full<Bytes>> {
    let size = response.body().size_hint().exact();
    let status = response.status();
    if !status.is_informational()
        && status != StatusCode::NO_CONTENT
        && status != StatusCode::NOT_MODIFIED
        && !response.headers().contains_key(CONTENT_LENGTH)
        && !response.headers().contains_key(TRANSFER_ENCODING)
        && let Some(size) = size.filter(|size| native_get || *size != 0)
    {
        response.headers_mut().insert(CONTENT_LENGTH, size.into());
    }
    *response.body_mut() = Full::new(Bytes::new());
    response
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parity_tests;

#[cfg(test)]
mod policy_tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::collections::BTreeSet;

    #[test]
    fn head_aliases_only_declared_public_gets_and_preserves_go_methods() {
        for (pattern, methods) in ROUTES {
            let path = pattern
                .replace("{token}", &"a".repeat(32))
                .replace("{message_id}", "message-1");
            let has_get = methods.split(", ").any(|m| m == "GET");
            assert_eq!(head_uses_get(&path), has_get, "{path}");
            if has_get {
                assert_eq!(
                    crate::ownership::route_owner("HEAD", &path),
                    crate::ownership::route_owner("GET", &path)
                );
            }
        }
        for path in [
            "/api/unknown",
            "/api/internal/usb-sms/inbox",
            "/api/internal/module-radio/query",
            "/api/v2/sms-export/",
            "/api/v2/sms-export/a/b",
        ] {
            assert!(!head_uses_get(path), "{path}");
            assert_eq!(admission(&Method::HEAD, path), Admission::Unknown);
        }
    }

    #[test]
    fn legacy_method_errors_use_the_union_without_changing_native_errors() {
        assert_eq!(admission(&Method::GET, "/api/health"), Admission::Allowed);
        assert_eq!(
            admission(&Method::HEAD, "/api/health"),
            Admission::HeadAsGet
        );
        assert_eq!(
            admission(&Method::POST, "/api/health"),
            Admission::MethodNotAllowed("GET, HEAD")
        );
        assert_eq!(admission(&Method::GET, "/api/status"), Admission::Allowed);
        assert_eq!(
            admission(&Method::HEAD, "/api/status"),
            Admission::HeadAsGet
        );
        assert_eq!(
            admission(&Method::POST, "/api/status"),
            Admission::MethodNotAllowed("GET, HEAD")
        );
        assert_eq!(
            admission(&Method::GET, "/api/esim/module-notes"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/esim/module-notes"),
            Admission::HeadAsGet
        );
        assert_eq!(
            admission(&Method::PUT, "/api/esim/module-notes"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::POST, "/api/esim/module-notes"),
            Admission::MethodNotAllowed("GET, HEAD, PUT")
        );
        assert_eq!(
            admission(&Method::POST, "/api/network/check-4g"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::POST, "/api/network/reboot-module"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::POST, "/api/network/usbnet"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/network/usbnet"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::GET, "/api/network/reboot-module"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::PUT, "/api/network/services-order"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::POST, "/api/network/services-order"),
            Admission::MethodNotAllowed("PUT")
        );
        assert_eq!(
            admission(&Method::GET, "/api/voice/enabled"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/voice/enabled"),
            Admission::HeadAsGet
        );
        assert_eq!(
            admission(&Method::POST, "/api/voice/enabled"),
            Admission::MethodNotAllowed("GET, HEAD")
        );
        assert_eq!(
            admission(&Method::GET, "/api/voice/enable"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::GET, "/api/network/check-4g"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::DELETE, "/api/module/flight-mode"),
            Admission::MethodNotAllowed("GET, HEAD, POST")
        );
        assert_eq!(
            admission(&Method::POST, "/api/reader/imei"),
            Admission::MethodNotAllowed("DELETE, GET, HEAD, PUT")
        );
        assert_eq!(
            admission(&Method::POST, "/api/reader/esim/profile"),
            Admission::MethodNotAllowed("DELETE, PATCH")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/sms/send"),
            Admission::MethodNotAllowed("POST")
        );
        for path in [
            "/api/v2/sms-export",
            "/api/v2/call/dtmf",
            "/api/v2/carrier/resolve",
            "/api/sms/forwarding/device-snapshot",
        ] {
            assert_eq!(admission(&Method::HEAD, path), Admission::Unknown, "{path}");
        }
        assert_eq!(
            admission(&Method::POST, "/api/v2/runtime"),
            Admission::Unknown
        );
        assert_eq!(
            admission(&Method::POST, "/api/multistandby/messages"),
            Admission::Unknown
        );
        assert_eq!(
            admission(&Method::from_bytes(b"get").unwrap(), "/api/sms"),
            Admission::MethodNotAllowed("GET, HEAD")
        );
        assert_eq!(admission(&Method::GET, "/api/sms/"), Admission::Unknown);
    }

    #[test]
    fn canonical_path_cleaning_matches_serve_mux_without_unescaping_segments() {
        for (input, expected) in [
            ("/api//sms", "/api/sms"),
            ("/api/./sms", "/api/sms"),
            ("/api/reader/../sms/", "/api/sms/"),
            ("/../../api/sms", "/api/sms"),
            ("/api/%2e%2e/sms", "/api/%2e%2e/sms"),
            ("/api/a%2Fb", "/api/a%2Fb"),
        ] {
            assert_eq!(clean_path(input), expected, "{input}");
        }
    }

    #[test]
    fn module_dtmf_public_route_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/call/dtmf"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/call/dtmf"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/call/dtmf"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn module_dial_public_route_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/call/dial"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/call/dial"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/call/dial"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn module_answer_and_hangup_public_routes_are_post_only() {
        for path in ["/api/call/answer", "/api/call/hangup"] {
            assert_eq!(admission(&Method::POST, path), Admission::Allowed, "{path}");
            assert_eq!(
                admission(&Method::GET, path),
                Admission::MethodNotAllowed("POST"),
                "{path}"
            );
            assert_eq!(
                admission(&Method::HEAD, path),
                Admission::MethodNotAllowed("POST"),
                "{path}"
            );
        }
    }

    #[test]
    fn module_call_status_is_get_and_head_only() {
        assert_eq!(
            admission(&Method::GET, "/api/call/status"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/call/status"),
            Admission::HeadAsGet
        );
        assert_eq!(
            admission(&Method::POST, "/api/call/status"),
            Admission::MethodNotAllowed("GET, HEAD")
        );
    }

    #[test]
    fn vowifi_call_status_aliases_are_get_and_head_only() {
        for path in [
            "/api/reader/vowifi/calls/status",
            "/api/vowifi/calls/status",
        ] {
            assert_eq!(admission(&Method::GET, path), Admission::Allowed);
            assert_eq!(admission(&Method::HEAD, path), Admission::HeadAsGet);
            assert!(matches!(
                admission(&Method::POST, path),
                Admission::MethodNotAllowed("GET, HEAD")
            ));
        }
    }

    #[test]
    fn sms_clear_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/sms/clear"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/sms/clear"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/sms/clear"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn sms_delete_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/sms/delete"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/sms/delete"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/sms/delete"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn sms_delete_sender_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/sms/delete-sender"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/sms/delete-sender"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/sms/delete-sender"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn call_mode_status_is_get_and_head_only() {
        assert_eq!(
            admission(&Method::GET, "/api/call-mode/status"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/call-mode/status"),
            Admission::HeadAsGet
        );
        assert_eq!(
            admission(&Method::POST, "/api/call-mode/status"),
            Admission::MethodNotAllowed("GET, HEAD")
        );
    }

    #[test]
    fn call_mode_runtime_download_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/call-mode/download"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/call-mode/download"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/call-mode/download"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn call_mode_retry_is_post_only() {
        assert_eq!(
            admission(&Method::POST, "/api/call-mode/retry"),
            Admission::Allowed
        );
        assert_eq!(
            admission(&Method::GET, "/api/call-mode/retry"),
            Admission::MethodNotAllowed("POST")
        );
        assert_eq!(
            admission(&Method::HEAD, "/api/call-mode/retry"),
            Admission::MethodNotAllowed("POST")
        );
    }

    #[test]
    fn canonical_redirect_preserves_method_and_raw_query() {
        let uri: Uri = "/api//sms?cursor=a%2Fb&x=1".parse().unwrap();
        let get = canonical_path_redirect(&Method::GET, &uri).unwrap();
        assert_eq!(get.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(get.headers()["location"], "/api/sms?cursor=a%2Fb&x=1");
        assert_eq!(get.headers()[CONTENT_TYPE], "text/html; charset=utf-8");

        let post = canonical_path_redirect(&Method::POST, &uri).unwrap();
        assert_eq!(post.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(post.headers()["location"], "/api/sms?cursor=a%2Fb&x=1");
        assert!(!post.headers().contains_key(CONTENT_TYPE));
        assert!(post.body().size_hint().exact().unwrap_or_default() == 0);

        let connect = canonical_path_redirect(&Method::CONNECT, &uri);
        assert!(connect.is_none());
        let escaped: Uri = "/api/%2e%2e/sms".parse().unwrap();
        assert!(canonical_path_redirect(&Method::GET, &escaped).is_none());
    }

    #[test]
    fn canonical_redirect_reescapes_cleaned_escaped_path_like_go_url() {
        // Independent Go1.26.7 findHandler builds URL.Path, not URL.RawPath,
        // from cleanPath(URL.EscapedPath()). Query escapes are not reescaped.
        for (raw, location) in [
            (
                "/api//%68ealth?x=a%2Fb&x=%252F",
                "/api/%2568ealth?x=a%2Fb&x=%252F",
            ),
            ("/api/./%2568ealth", "/api/%252568ealth"),
            ("/%61pi/x/../health?", "/%2561pi/health"),
            ("/api//health?", "/api/health"),
            ("/api//%2fhealth/", "/api/%252fhealth/"),
            ("/api//a'b", "/api/a%27b"),
        ] {
            for method in [Method::GET, Method::HEAD, Method::POST] {
                let uri = raw.parse().unwrap();
                let response = canonical_path_redirect(&method, &uri).unwrap();
                assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
                assert_eq!(response.headers()["location"], location, "{method} {raw}");
            }
        }
    }

    #[test]
    fn malformed_path_escapes_are_rejected_but_raw_query_and_valid_octets_are_opaque() {
        for raw in ["/api/%", "/api/%6", "/api/%6G", "/api//%GG"] {
            let response = invalid_path_response(&raw.parse().unwrap()).unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(response.headers()["connection"], "close");
        }
        for raw in ["/api/health?x=%GG", "/api/%00", "/api/%FF", "/api/%256G"] {
            assert!(
                invalid_path_response(&raw.parse().unwrap()).is_none(),
                "{raw}"
            );
        }
        for (raw, value) in [
            ("%61bc", "abc"),
            ("a%2Fb", "a/b"),
            ("%2561", "%61"),
            ("a+b", "a+b"),
            ("%E4%B8%AD", "中"),
        ] {
            assert_eq!(path_value(raw).as_deref(), Some(value));
        }
        assert!(path_value("%GG").is_none());
        assert!(path_value("%FF").is_none());
    }

    #[test]
    fn public_alias_matches_literal_segments_once_and_preserves_uri_parts() {
        let original: Uri = "http://example.invalid:8080/%61pi/%68ealth?x=a%2Fb&x=%2561&empty="
            .parse()
            .unwrap();
        let alias = public_alias_uri(&original).unwrap();
        assert_eq!(alias.path(), "/api/health");
        assert_eq!(alias.query(), original.query());
        assert_eq!(alias.scheme(), original.scheme());
        assert_eq!(alias.authority(), original.authority());
        assert!(public_alias_uri(&alias).is_none());
        assert_eq!(original.path(), "/%61pi/%68ealth");
        let empty_query: Uri = "/api/%68ealth?".parse().unwrap();
        assert_eq!(
            public_alias_uri(&empty_query).unwrap().to_string(),
            "/api/health?"
        );
    }

    #[test]
    fn public_alias_never_promotes_encoded_separators_or_double_escapes() {
        for raw in [
            "/api%2Fhealth",
            "/api%2fhealth",
            "/api/%2Fhealth",
            "/api/%68ealth%2f",
            "/api/%2e%2e/health",
            "/%2561pi/health",
            "/api/%2568ealth",
            "/api/%3fhealth",
            "/api/%23health",
            "/api/%00health",
            "/api/%ffhealth",
        ] {
            let uri: Uri = raw.parse().unwrap();
            assert!(public_alias_uri(&uri).is_none(), "{raw}");
        }
    }

    #[test]
    fn public_alias_preserves_original_raw_wildcard_tail() {
        let original: Uri = "/%61pi/multistandby/operations/%2561%2Fb%3Fc?x=%252F"
            .parse()
            .unwrap();
        let alias = public_alias_uri(&original).unwrap();
        assert_eq!(alias.path(), "/api/multistandby/operations/%2561%2Fb%3Fc");
        assert_eq!(alias.query(), original.query());
        assert_eq!(
            admission(&Method::OPTIONS, alias.path()),
            Admission::MethodNotAllowed("GET, HEAD")
        );
        assert!(public_alias_uri(&alias).is_none());
        assert!(public_alias_uri(&"/%61pi/multistandby/operations/a/b".parse().unwrap()).is_none());
        assert!(public_alias_uri(&"/%61pi/multistandby/operations/".parse().unwrap()).is_none());
    }

    #[test]
    fn public_alias_leaves_unknown_private_and_retired_capabilities_untouched() {
        for raw in [
            "/api/%69nternal/reader-service",
            "/%61pi/internal/reader-pcsc/connect",
            "/api/internal/%6dodule-radio/submit",
            "/api/internal/%72eader-service",
            "/api/%69nternal/ordinary-vowifi/stop",
            "/api/%69nternal/multistandby/reader-reservation/operation/prepare",
            "/api/no-such-%68ttp-route",
            "/api/%68ealth/",
            "/api/%68ealth/child",
        ] {
            let uri: Uri = raw.parse().unwrap();
            assert!(public_alias_uri(&uri).is_none(), "{raw}");
        }
    }

    #[test]
    fn public_alias_refuses_invalid_percent_sequences_without_repair() {
        for segment in ["%", "%6", "%6G", "%G6", "health%", "health%0"] {
            assert!(decode_alias_segment(segment).is_none(), "{segment}");
            if let Ok(uri) = format!("/api/{segment}").parse::<Uri>() {
                assert!(public_alias_uri(&uri).is_none(), "{segment}");
            }
        }
        let raw_tail: Uri = "/%61pi/multistandby/operations/%6G".parse().unwrap();
        assert!(public_alias_uri(&raw_tail).is_none());
    }

    #[test]
    fn public_alias_keeps_raw_canonical_redirects_before_literal_matching() {
        for raw in [
            "/api//%68ealth?x=a%2Fb",
            "/api/./%68ealth",
            "/api/x/../%68ealth",
        ] {
            let uri: Uri = raw.parse().unwrap();
            assert!(public_alias_uri(&uri).is_none(), "{raw}");
            assert!(canonical_path_redirect(&Method::GET, &uri).is_some());
            assert!(canonical_path_redirect(&Method::CONNECT, &uri).is_none());
        }
    }

    #[test]
    fn public_alias_precedes_head_and_method_classification_without_changing_credentials_or_body() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            let mut request = hyper::Request::builder()
                .method(method.clone())
                .uri("/api/%68ealth?x=a%2Fb")
                .header(crate::APP_TOKEN_HEADER, "public-alias-fixture")
                .body(vec![1_u8, 2, 3])
                .unwrap();
            let headers = request.headers().clone();
            let alias = public_alias_uri(request.uri()).unwrap();
            *request.uri_mut() = alias;
            assert_eq!(request.method(), &method);
            assert_eq!(request.headers(), &headers);
            assert_eq!(request.body().as_slice(), &[1_u8, 2, 3]);
            assert!(crate::token_matches(&request, Some("public-alias-fixture")));
            assert!(!crate::token_matches(&request, Some("another-app-token")));
            assert!(!crate::multistandby_store::internal_authenticated(
                &request,
                "private-fixture"
            ));
            assert!(head_uses_get(request.uri().path()));
            let expected = if method == Method::HEAD {
                Admission::HeadAsGet
            } else if method == Method::OPTIONS {
                Admission::MethodNotAllowed("GET, HEAD")
            } else {
                Admission::Allowed
            };
            assert_eq!(admission(request.method(), request.uri().path()), expected);
            assert!(public_alias_uri(request.uri()).is_none());
        }
    }

    #[test]
    fn public_alias_preserves_module_mutation_gate_classification() {
        let uri: Uri = "/api/%63all-mode/restore?x=%252F".parse().unwrap();
        let alias = public_alias_uri(&uri).unwrap();
        assert_eq!(alias.path(), "/api/call-mode/restore");
        assert!(crate::module_recovery::owns_public(
            &Method::POST,
            alias.path()
        ));
        assert_eq!(admission(&Method::POST, alias.path()), Admission::Allowed);
        assert_eq!(alias.query(), uri.query());
    }

    #[test]
    fn method_table_is_bounded_unambiguous_and_preserves_sorted_allow() {
        let mut paths = BTreeSet::new();
        for (path, methods) in ROUTES {
            assert!(paths.insert(*path));
            assert!(!path.starts_with("/api/internal/"));
            let values = methods.split(", ").collect::<Vec<_>>();
            let sorted = values
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            assert_eq!(values, sorted);
            assert_eq!(values.contains(&"GET"), values.contains(&"HEAD"));
            for method in values {
                Method::from_bytes(method.as_bytes()).unwrap();
            }
        }
        assert_eq!(
            LEGACY_METHOD_PATHS
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            LEGACY_METHOD_PATHS.len()
        );
        for path in LEGACY_METHOD_PATHS {
            assert!(paths.contains(path));
        }
    }

    #[test]
    fn rust_only_control_intents_are_post_only_without_legacy_mux_parity() {
        for path in [
            "/api/multistandby/control-intents",
            "/api/multistandby/control-intents/ack",
            "/api/reader/vowifi/reconcile",
            "/api/vowifi/reconcile",
        ] {
            assert_eq!(allowed_methods(path), Some("POST"));
            assert_eq!(admission(&Method::POST, path), Admission::Allowed);
            assert_eq!(admission(&Method::GET, path), Admission::Unknown);
            assert!(!legacy_method_path(path));
        }
    }

    #[tokio::test]
    async fn head_representation_length_and_bodyless_statuses_are_preserved() {
        let response = Response::new(Full::new(Bytes::from_static(b"representation")));
        let response = without_head_body(response, true);
        assert_eq!(response.headers()[CONTENT_LENGTH], "14");
        assert!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );

        let upstream = Response::builder()
            .header(CONTENT_LENGTH, "9000")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let upstream = without_head_body(upstream, false);
        assert_eq!(upstream.headers()[CONTENT_LENGTH], "9000");
        for status in [
            StatusCode::OK,
            StatusCode::TEMPORARY_REDIRECT,
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
        ] {
            let original = Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = without_head_body(original, false);
            assert!(!response.headers().contains_key(CONTENT_LENGTH));
        }
        for status in [StatusCode::NO_CONTENT, StatusCode::NOT_MODIFIED] {
            let original = Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap();
            assert!(
                !without_head_body(original, true)
                    .headers()
                    .contains_key(CONTENT_LENGTH)
            );
        }
        let chunked = Response::builder()
            .header(TRANSFER_ENCODING, "chunked")
            .body(Full::new(Bytes::from_static(b"body")))
            .unwrap();
        assert!(
            !without_head_body(chunked, true)
                .headers()
                .contains_key(CONTENT_LENGTH)
        );
    }
}
