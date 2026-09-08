//! W3C trace context over MCP request metadata, mirroring
//! `src/observability/trace-context.ts`.
//!
//! Divergence: TS scopes the context with `AsyncLocalStorage`; the server
//! sets it with a thread-local around each request handler instead (axum
//! handlers + `spawn_blocking` daemon calls never share implicit async
//! context the same way).

use std::cell::RefCell;

/// Validated W3C `traceparent` header value.
///
/// Private field: only [`parse_traceparent`] builds one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceParent(String);

impl TraceParent {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 32-hex trace id embedded in the `traceparent`.
    pub fn trace_id(&self) -> &str {
        self.0.split('-').nth(1).unwrap_or("")
    }
}

impl std::fmt::Display for TraceParent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Request id for log correlation (`RequestId` newtype, M2).
///
/// Private field: only [`request_id`] builds one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Generates a fresh request id (mirrors TS `requestId`).
pub fn request_id() -> RequestId {
    RequestId(uuid::Uuid::new_v4().to_string())
}

/// Ambient trace context for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub traceparent: TraceParent,
    pub tracestate: Option<String>,
    pub baggage: Option<String>,
}

impl TraceContext {
    /// The 32-hex trace id.
    pub fn trace_id(&self) -> &str {
        self.traceparent.trace_id()
    }
}

thread_local! {
    static CURRENT_TRACE: RefCell<Option<TraceContext>> = const { RefCell::new(None) };
}

/// Returns the ambient trace context, if any.
pub fn current_trace_context() -> Option<TraceContext> {
    CURRENT_TRACE.with(|cell| cell.borrow().clone())
}

/// Runs `operation` with `context` as the ambient trace context.
pub fn run_with_trace_context<T>(
    context: Option<TraceContext>,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = CURRENT_TRACE.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), context));
    struct Restore(Option<TraceContext>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_TRACE.with(|cell| *cell.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(previous);
    operation()
}

/// Extracts trace context from an MCP request body (`params._meta`).
pub fn trace_context_from_mcp_body(body: &serde_json::Value) -> Option<TraceContext> {
    let meta = body.get("params")?.get("_meta")?;
    trace_context_from_mcp_meta(meta)
}

/// Extracts trace context from an MCP `_meta` object.
pub fn trace_context_from_mcp_meta(meta: &serde_json::Value) -> Option<TraceContext> {
    let object = meta.as_object()?;
    let traceparent = object.get("traceparent")?.as_str()?;
    let parent = parse_traceparent(traceparent)?;
    let tracestate = object.get("tracestate").and_then(valid_tracestate);
    let baggage = object.get("baggage").and_then(valid_baggage);
    Some(TraceContext {
        traceparent: parent,
        tracestate,
        baggage,
    })
}

/// Outgoing propagation headers for the ambient context.
pub fn trace_headers() -> Vec<(String, String)> {
    let Some(context) = current_trace_context() else {
        return Vec::new();
    };
    let mut headers = vec![("traceparent".to_owned(), context.traceparent.to_string())];
    if let Some(tracestate) = context.tracestate {
        headers.push(("tracestate".to_owned(), tracestate));
    }
    if let Some(baggage) = context.baggage {
        headers.push(("baggage".to_owned(), baggage));
    }
    headers
}

/// Validates a `traceparent`: `version-traceid-spanid-flags`, rejecting
/// version `ff`, all-zero trace/span ids, and extensions on version `00`.
fn parse_traceparent(value: &str) -> Option<TraceParent> {
    let mut parts = value.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    if version.len() != 2 || !is_lower_hex(version, 2) {
        return None;
    }
    if !is_lower_hex(trace_id, 32) || !is_lower_hex(span_id, 16) || !is_lower_hex(flags, 2) {
        return None;
    }
    if !rest
        .iter()
        .all(|part| !part.is_empty() && is_lower_hex(part, part.len()))
    {
        return None;
    }
    if version == "ff" {
        return None;
    }
    if version == "00" && !rest.is_empty() {
        return None;
    }
    if trace_id.chars().all(|c| c == '0') || span_id.chars().all(|c| c == '0') {
        return None;
    }
    Some(TraceParent(value.to_owned()))
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
/// Printable-ASCII bound plus optional horizontal tab, mirroring TS
/// `boundedHeader`.
fn bounded_header(value: &serde_json::Value, max_length: usize, allow_tab: bool) -> Option<String> {
    let text = value.as_str()?;
    if text.len() > max_length {
        return None;
    }
    for c in text.chars() {
        let code = c as u32;
        if code > 0x7e || (code < 0x20 && !(allow_tab && code == 0x09)) {
            return None;
        }
    }
    Some(text.to_owned())
}

/// Validates a `tracestate` header (512 chars, at most 32 members, valid
/// keys/values, no duplicates), mirroring TS `validTracestate`.
fn valid_tracestate(value: &serde_json::Value) -> Option<String> {
    let header = bounded_header(value, 512, false)?;
    let members: Vec<&str> = header.split(',').collect();
    if members.len() > 32 {
        return None;
    }
    let mut keys = std::collections::HashSet::new();
    for raw in members {
        let member = raw.trim();
        let separator = member.find('=')?;
        if separator == 0 {
            return None;
        }
        let key = &member[..separator];
        let item = &member[separator + 1..];
        if !valid_tracestate_key(key)
            || item.is_empty()
            || item.len() > 256
            || !valid_tracestate_value(item)
            || item.ends_with(' ')
            || !keys.insert(key)
        {
            return None;
        }
    }
    Some(header)
}

fn valid_tracestate_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes[0] == b'@' {
        return false;
    }
    if let Some(at) = key.find('@') {
        let (tenant, rest) = key.split_at(at);
        let vendor = &rest[1..];
        // `[a-z0-9][a-z0-9_\-*/]{0,240}@[a-z][a-z0-9_\-*/]{0,13}`
        valid_simple_key(tenant, 241)
            && valid_simple_key(vendor, 14)
            && tenant.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && vendor.starts_with(|c: char| c.is_ascii_lowercase())
    } else {
        // `[a-z][a-z0-9_\-*/]{0,255}`
        key.len() <= 256
            && key.starts_with(|c: char| c.is_ascii_lowercase())
            && key.bytes().all(tracestate_key_char)
    }
}

fn valid_simple_key(key: &str, max: usize) -> bool {
    !key.is_empty() && key.len() <= max && key.bytes().all(tracestate_key_char)
}

fn tracestate_key_char(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'*' | b'/')
}

fn valid_tracestate_value(item: &str) -> bool {
    item.bytes().all(|b| {
        (0x20..=0x2b).contains(&b) || (0x2d..=0x3c).contains(&b) || (0x3e..=0x7e).contains(&b)
    })
}

/// Validates a `baggage` header (8192 chars, at most 64 members),
/// mirroring TS `validBaggage`.
fn valid_baggage(value: &serde_json::Value) -> Option<String> {
    let header = bounded_header(value, 8192, true)?;
    let members: Vec<&str> = header.split(',').collect();
    if members.len() > 64 {
        return None;
    }
    for raw in members {
        let mut segments = raw.trim().split(';');
        let pair = segments.next().unwrap_or("");
        let separator = pair.find('=').unwrap_or(0);
        if separator == 0 {
            return None;
        }
        let key = pair[..separator].trim();
        let pair_value = pair[separator + 1..].trim();
        if !is_http_token(key) || has_invalid_baggage_characters(pair_value, &[',']) {
            return None;
        }
        for raw_property in segments {
            let property = raw_property.trim();
            let (key, property_value) = match property.find('=') {
                Some(index) => (property[..index].trim(), property[index + 1..].trim()),
                None => (property, ""),
            };
            if !is_http_token(key) || has_invalid_baggage_characters(property_value, &[';', ',']) {
                return None;
            }
        }
    }
    Some(header)
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn has_invalid_baggage_characters(value: &str, delimiters: &[char]) -> bool {
    value.chars().any(|c| {
        let code = c as u32;
        code <= 0x20 || code == 0x22 || code == 0x5c || code == 0x7f || delimiters.contains(&c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(traceparent: &str) -> serde_json::Value {
        serde_json::json!({ "traceparent": traceparent })
    }

    #[test]
    fn inject_extract_round_trip() {
        let context = trace_context_from_mcp_meta(&serde_json::json!({
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "tracestate": "rojo=00f067aa0ba902b7,congo=t61rcWkgMzE",
            "baggage": "userId=alice,serverNode=DF%2028",
        }))
        .expect("valid context");
        assert_eq!(context.trace_id(), "4bf92f3577b34da6a3ce929d0e0e4736");
        run_with_trace_context(Some(context), || {
            let headers = trace_headers();
            assert!(headers.iter().any(|(k, v)| k == "traceparent"
                && v == "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"));
            assert!(headers.iter().any(|(k, _)| k == "tracestate"));
            assert!(headers.iter().any(|(k, _)| k == "baggage"));
        });
        assert!(current_trace_context().is_none());
    }

    #[test]
    fn body_extracts_from_params_meta() {
        let body = serde_json::json!({
            "params": { "_meta": meta("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01") }
        });
        assert!(trace_context_from_mcp_body(&body).is_some());
        assert!(trace_context_from_mcp_body(&serde_json::json!({})).is_none());
    }

    #[test]
    fn invalid_traceparents_rejected() {
        for bad in [
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "not-a-traceparent",
        ] {
            assert!(trace_context_from_mcp_meta(&meta(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn invalid_tracestate_dropped_but_parent_kept() {
        let context = trace_context_from_mcp_meta(&serde_json::json!({
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "tracestate": "no-equals-here",
        }))
        .expect("parent valid");
        assert!(context.tracestate.is_none());
    }
}
