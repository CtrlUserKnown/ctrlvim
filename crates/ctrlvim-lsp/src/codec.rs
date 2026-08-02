//! LSP's wire format: JSON-RPC framed with an HTTP-style `Content-Length`
//! header, the same shape `textDocument/*` messages have used since the
//! protocol's first version.
//!
//! Bytes arrive from [`ctrlvim_async::Event::ProcessStdout`] in whatever
//! chunks the OS pipe happens to hand back — never guaranteed to align with a
//! message boundary, and sometimes several small messages arrive in one
//! chunk. [`Decoder`] buffers across calls and yields only whole messages.

use serde_json::Value;

/// Encode one JSON-RPC message with its `Content-Length` header.
pub fn encode(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).expect("a serde_json::Value always serializes");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Accumulates raw stdout bytes and hands back complete JSON-RPC messages as
/// they close, holding onto everything else (a partial header, or a body
/// that hasn't fully arrived yet) for the next call.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Decoder::default()
    }

    /// Feed newly-read bytes in and drain every message that's now complete.
    /// A message this crate can't make sense of (malformed header, body that
    /// isn't valid JSON) is dropped rather than wedging the whole decoder —
    /// one confused reply from a server should not take down the connection.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some((value, consumed)) = self.try_take_one() {
            if let Some(value) = value {
                out.push(value);
            }
            self.buf.drain(..consumed);
        }
        out
    }

    /// One attempt at slicing a complete message off the front of `self.buf`.
    /// Returns `(parsed_or_none, bytes_consumed)` — `parsed_or_none` is
    /// `None` for a header naming a body that parsed to something other than
    /// valid JSON (still consumed, so the stream doesn't get stuck), and the
    /// whole call is `None` when the buffer doesn't yet hold a full message.
    fn try_take_one(&self) -> Option<(Option<Value>, usize)> {
        let header_end = find_subslice(&self.buf, b"\r\n\r\n")?;
        let header = std::str::from_utf8(&self.buf[..header_end]).ok()?;
        let len: usize = header
            .split("\r\n")
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .and_then(|v| v.trim().parse().ok())?;

        let body_start = header_end + 4;
        let body_end = body_start.checked_add(len)?;
        if self.buf.len() < body_end {
            return None; // body hasn't fully arrived yet
        }
        let value = serde_json::from_slice(&self.buf[body_start..body_end]).ok();
        Some((value, body_end))
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_prefixes_the_body_with_its_byte_length() {
        let msg = json!({"a": 1});
        let body = serde_json::to_vec(&msg).unwrap();
        let out = encode(&msg);
        let expected_header = format!("Content-Length: {}\r\n\r\n", body.len());
        assert!(out.starts_with(expected_header.as_bytes()));
        assert_eq!(&out[expected_header.len()..], body.as_slice());
    }

    #[test]
    fn decodes_one_message_fed_whole() {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "result": null});
        let mut dec = Decoder::new();
        let out = dec.feed(&encode(&msg));
        assert_eq!(out, vec![msg]);
    }

    #[test]
    fn decodes_a_message_split_across_many_small_reads() {
        let msg = json!({"method": "initialized", "params": {}});
        let bytes = encode(&msg);
        let mut dec = Decoder::new();
        let mut out = Vec::new();
        for byte in &bytes {
            out.extend(dec.feed(std::slice::from_ref(byte)));
        }
        assert_eq!(out, vec![msg]);
    }

    #[test]
    fn decodes_two_messages_arriving_in_one_chunk() {
        let a = json!({"id": 1});
        let b = json!({"id": 2});
        let mut bytes = encode(&a);
        bytes.extend(encode(&b));
        let mut dec = Decoder::new();
        assert_eq!(dec.feed(&bytes), vec![a, b]);
    }

    #[test]
    fn a_partial_message_waits_for_the_rest() {
        let msg = json!({"id": 1, "result": "ok"});
        let bytes = encode(&msg);
        let mut dec = Decoder::new();
        let (head, tail) = bytes.split_at(bytes.len() - 3);
        assert!(dec.feed(head).is_empty(), "the body hasn't fully arrived");
        assert_eq!(dec.feed(tail), vec![msg]);
    }

    #[test]
    fn header_only_arriving_first_still_waits() {
        let msg = json!({"ok": true});
        let bytes = encode(&msg);
        let header_end = find_subslice(&bytes, b"\r\n\r\n").unwrap() + 4;
        let mut dec = Decoder::new();
        assert!(dec.feed(&bytes[..header_end]).is_empty());
        assert_eq!(dec.feed(&bytes[header_end..]), vec![msg]);
    }
}
