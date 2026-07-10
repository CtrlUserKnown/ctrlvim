//! msgpack-RPC codec and dispatcher — the replacement for
//! `msgpack_rpc/channel.c` + `packer.c`/`unpacker.c`.
//!
//! Rather than port the hand-rolled incremental unpacker, we use the `rmpv`
//! crate for the wire format. This module encodes/decodes the msgpack-RPC
//! message types and dispatches requests through a handler closure (wired to
//! `nvim-api::call` at the top level), keeping this crate free of an API
//! dependency so the layering stays clean.

use nvim_types::Object;
use rmpv::Value;
use std::collections::BTreeMap;

/// A decoded msgpack-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `[0, msgid, method, params]`
    Request {
        msgid: u32,
        method: String,
        params: Vec<Object>,
    },
    /// `[1, msgid, error, result]`
    Response {
        msgid: u32,
        error: Option<Object>,
        result: Object,
    },
    /// `[2, method, params]`
    Notification { method: String, params: Vec<Object> },
}

/// Encode a message to msgpack bytes.
pub fn encode(msg: &Message) -> Vec<u8> {
    let value = match msg {
        Message::Request { msgid, method, params } => Value::Array(vec![
            Value::from(0),
            Value::from(*msgid),
            Value::from(method.as_str()),
            object_to_value(&Object::Array(params.clone())),
        ]),
        Message::Response { msgid, error, result } => Value::Array(vec![
            Value::from(1),
            Value::from(*msgid),
            error.as_ref().map(object_to_value).unwrap_or(Value::Nil),
            object_to_value(result),
        ]),
        Message::Notification { method, params } => Value::Array(vec![
            Value::from(2),
            Value::from(method.as_str()),
            object_to_value(&Object::Array(params.clone())),
        ]),
    };
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &value).expect("encode to Vec cannot fail");
    buf
}

/// Decode msgpack bytes into a message.
pub fn decode(bytes: &[u8]) -> Result<Message, String> {
    let mut cursor = bytes;
    let value = rmpv::decode::read_value(&mut cursor).map_err(|e| e.to_string())?;
    let arr = match value {
        Value::Array(a) => a,
        _ => return Err("rpc message is not an array".into()),
    };
    let kind = arr.first().and_then(Value::as_u64).ok_or("missing message type")?;
    match kind {
        0 => {
            let msgid = arr.get(1).and_then(Value::as_u64).ok_or("missing msgid")? as u32;
            let method = arr.get(2).and_then(Value::as_str).ok_or("missing method")?.to_string();
            let params = value_to_params(arr.get(3))?;
            Ok(Message::Request { msgid, method, params })
        }
        1 => {
            let msgid = arr.get(1).and_then(Value::as_u64).ok_or("missing msgid")? as u32;
            let error = match arr.get(2) {
                Some(Value::Nil) | None => None,
                Some(v) => Some(value_to_object(v)),
            };
            let result = arr.get(3).map(value_to_object).unwrap_or(Object::Nil);
            Ok(Message::Response { msgid, error, result })
        }
        2 => {
            let method = arr.get(1).and_then(Value::as_str).ok_or("missing method")?.to_string();
            let params = value_to_params(arr.get(2))?;
            Ok(Message::Notification { method, params })
        }
        other => Err(format!("unknown rpc message type {other}")),
    }
}

fn value_to_params(v: Option<&Value>) -> Result<Vec<Object>, String> {
    match v {
        Some(Value::Array(items)) => Ok(items.iter().map(value_to_object).collect()),
        Some(Value::Nil) | None => Ok(Vec::new()),
        _ => Err("params must be an array".into()),
    }
}

/// Handle one incoming message, dispatching Requests/Notifications through
/// `handler`. Returns response bytes for a Request (`None` for notifications).
pub fn handle<F>(bytes: &[u8], mut handler: F) -> Result<Option<Vec<u8>>, String>
where
    F: FnMut(&str, Vec<Object>) -> nvim_types::Result<Object>,
{
    match decode(bytes)? {
        Message::Request { msgid, method, params } => {
            let (error, result) = match handler(&method, params) {
                Ok(obj) => (None, obj),
                Err(e) => (Some(Object::str(e.to_string())), Object::Nil),
            };
            Ok(Some(encode(&Message::Response { msgid, error, result })))
        }
        Message::Notification { method, params } => {
            let _ = handler(&method, params);
            Ok(None)
        }
        Message::Response { .. } => Ok(None),
    }
}

// --- rmpv::Value <-> Object ---

fn object_to_value(obj: &Object) -> Value {
    match obj {
        Object::Nil => Value::Nil,
        Object::Boolean(b) => Value::Boolean(*b),
        Object::Integer(i) => Value::from(*i),
        Object::Float(f) => Value::from(*f),
        Object::String(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => Value::from(s),
            Err(_) => Value::Binary(bytes.clone()),
        },
        Object::Array(items) => Value::Array(items.iter().map(object_to_value).collect()),
        Object::Dict(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (Value::from(k.as_str()), object_to_value(v)))
                .collect(),
        ),
        // Handles serialize as their integer id on the wire (Neovim uses ext
        // types; integer is a faithful-enough demo encoding).
        Object::Buffer(b) => Value::from(b.0),
        Object::Window(w) => Value::from(w.0),
        Object::Tabpage(t) => Value::from(t.0),
        Object::LuaRef(_) => Value::Nil, // functions are not sendable over RPC
    }
}

fn value_to_object(value: &Value) -> Object {
    match value {
        Value::Nil => Object::Nil,
        Value::Boolean(b) => Object::Boolean(*b),
        Value::Integer(i) => {
            if let Some(u) = i.as_i64() {
                Object::Integer(u)
            } else {
                Object::Integer(0)
            }
        }
        Value::F32(f) => Object::Float(*f as f64),
        Value::F64(f) => Object::Float(*f),
        Value::String(s) => match s.as_str() {
            Some(st) => Object::str(st),
            None => Object::String(s.as_bytes().to_vec()),
        },
        Value::Binary(b) => Object::String(b.clone()),
        Value::Array(items) => Object::Array(items.iter().map(value_to_object).collect()),
        Value::Map(pairs) => {
            let mut map = BTreeMap::new();
            for (k, v) in pairs {
                if let Some(key) = k.as_str() {
                    map.insert(key.to_string(), value_to_object(v));
                }
            }
            Object::Dict(map)
        }
        Value::Ext(_, _) => Object::Nil,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let msg = Message::Request {
            msgid: 5,
            method: "nvim_get_current_line".into(),
            params: vec![Object::Integer(0)],
        };
        let bytes = encode(&msg);
        assert_eq!(decode(&bytes).unwrap(), msg);
    }

    #[test]
    fn notification_roundtrip() {
        let msg = Message::Notification {
            method: "redraw".into(),
            params: vec![Object::str("flush")],
        };
        let bytes = encode(&msg);
        assert_eq!(decode(&bytes).unwrap(), msg);
    }

    #[test]
    fn handle_dispatches_request_and_encodes_response() {
        let req = encode(&Message::Request {
            msgid: 1,
            method: "add".into(),
            params: vec![Object::Integer(2), Object::Integer(3)],
        });
        let resp_bytes = handle(&req, |method, params| {
            assert_eq!(method, "add");
            let sum: i64 = params.iter().filter_map(Object::as_int).sum();
            Ok(Object::Integer(sum))
        })
        .unwrap()
        .expect("request yields a response");

        match decode(&resp_bytes).unwrap() {
            Message::Response { msgid, error, result } => {
                assert_eq!(msgid, 1);
                assert!(error.is_none());
                assert_eq!(result, Object::Integer(5));
            }
            _ => panic!("expected a response"),
        }
    }

    #[test]
    fn handler_error_becomes_response_error() {
        let req = encode(&Message::Request {
            msgid: 9,
            method: "boom".into(),
            params: vec![],
        });
        let resp = handle(&req, |_, _| Err(nvim_types::Error::exception("kaboom"))).unwrap().unwrap();
        match decode(&resp).unwrap() {
            Message::Response { error, .. } => {
                assert!(error.unwrap().as_str().unwrap().contains("kaboom"));
            }
            _ => panic!("expected response"),
        }
    }

    #[test]
    fn dict_params_survive_roundtrip() {
        let mut d = BTreeMap::new();
        d.insert("pattern".to_string(), Object::str("*.rs"));
        d.insert("n".to_string(), Object::Integer(7));
        let msg = Message::Request { msgid: 2, method: "x".into(), params: vec![Object::Dict(d.clone())] };
        let back = decode(&encode(&msg)).unwrap();
        assert_eq!(back, msg);
    }
}
