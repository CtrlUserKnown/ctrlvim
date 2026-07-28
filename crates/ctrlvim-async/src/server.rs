//! msgpack-RPC transport — the replacement for `msgpack_rpc/server.c`.
//!
//! [`crate::rpc`] has the codec and the dispatcher, but a codec with no socket
//! behind it can only ever talk to itself. This module is the listener: it
//! accepts connections on a Unix socket, decodes msgpack-RPC messages off each
//! stream, and hands them to a handler — which is what lets an external client
//! (a GUI, a test harness, a plugin host in another process) attach the way
//! `$NVIM_LISTEN_ADDRESS` lets one attach to Neovim.
//!
//! Streaming, not framing: msgpack-RPC has no length prefix, so a reader has to
//! attempt a decode and wait for more bytes when the buffer holds only part of
//! a message. [`Connection::feed`] implements exactly that.

use std::io;
use std::path::{Path, PathBuf};

use crate::rpc::{decode, encode, Message};

/// Accumulates bytes from a stream and yields whole messages as they complete.
#[derive(Default)]
pub struct Connection {
    buf: Vec<u8>,
}

impl Connection {
    pub fn new() -> Self {
        Connection::default()
    }

    /// Add received bytes and return every message that is now complete.
    ///
    /// A partial message stays buffered; a malformed one is reported and the
    /// buffer is cleared, since there is no way to resynchronize a msgpack
    /// stream mid-message.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Message>, String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.buf.is_empty() {
                break;
            }
            match decode_prefix(&self.buf) {
                DecodeStep::Done(msg, used) => {
                    self.buf.drain(..used);
                    out.push(msg);
                }
                DecodeStep::Incomplete => break,
                DecodeStep::Invalid(e) => {
                    self.buf.clear();
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// Bytes still buffered awaiting the rest of a message.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

enum DecodeStep {
    Done(Message, usize),
    Incomplete,
    Invalid(String),
}

/// Try to decode one message from the front of `buf`, reporting how many bytes
/// it consumed.
fn decode_prefix(buf: &[u8]) -> DecodeStep {
    let mut cursor = io::Cursor::new(buf);
    match rmpv::decode::read_value(&mut cursor) {
        // The decoded value is discarded: this pass only measures how many
        // bytes one message occupies, so the real decoder below sees a
        // complete frame.
        Ok(_) => {
            let used = cursor.position() as usize;
            // Re-encode just this message's bytes for the existing decoder, so
            // the wire format has exactly one implementation.
            match decode(&buf[..used]) {
                Ok(msg) => DecodeStep::Done(msg, used),
                Err(e) => DecodeStep::Invalid(e),
            }
        }
        Err(e) => {
            // `rmpv` reports a truncated buffer as an I/O error; anything else
            // is genuinely malformed.
            let msg = e.to_string();
            if is_truncation(&e) {
                DecodeStep::Incomplete
            } else {
                DecodeStep::Invalid(msg)
            }
        }
    }
}

fn is_truncation(e: &rmpv::decode::Error) -> bool {
    match e {
        rmpv::decode::Error::InvalidMarkerRead(io_err)
        | rmpv::decode::Error::InvalidDataRead(io_err) => {
            io_err.kind() == io::ErrorKind::UnexpectedEof
        }
        _ => false,
    }
}

/// A listening RPC server. Dropping it removes the socket file.
pub struct RpcServer {
    listener: tokio::net::UnixListener,
    path: PathBuf,
}

impl RpcServer {
    /// Bind a Unix socket at `path`, replacing a stale socket left by a crashed
    /// process (a refusal to start because of a leftover file would be worse
    /// than the small risk of stealing a live address).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        Ok(RpcServer { listener, path })
    }

    /// The socket's path, for advertising to clients.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serve connections until the future is dropped, dispatching each request
    /// through `handler` and writing the response back.
    ///
    /// `handler` is `FnMut` and shared across connections, which keeps the
    /// editor single-threaded: requests are serialized, exactly as Neovim
    /// serializes API calls onto its main loop.
    pub async fn serve<F>(&self, mut handler: F) -> io::Result<()>
    where
        F: FnMut(&str, &[ctrlvim_types::Object]) -> Result<ctrlvim_types::Object, String>,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let mut conn = Connection::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = match stream.read(&mut chunk).await {
                    Ok(0) => break, // client hung up
                    Ok(n) => n,
                    Err(_) => break,
                };
                let messages = match conn.feed(&chunk[..n]) {
                    Ok(m) => m,
                    Err(_) => break, // unrecoverable stream desync
                };
                for msg in messages {
                    let reply = match msg {
                        Message::Request { msgid, method, params } => {
                            let (error, result) = match handler(&method, &params) {
                                Ok(v) => (None, v),
                                Err(e) => (Some(ctrlvim_types::Object::str(e)), ctrlvim_types::Object::Nil),
                            };
                            Some(Message::Response { msgid, error, result })
                        }
                        // Notifications are fire-and-forget by definition.
                        Message::Notification { method, params } => {
                            let _ = handler(&method, &params);
                            None
                        }
                        // A response arriving at a server is a protocol error;
                        // ignoring it is friendlier than dropping the client.
                        Message::Response { .. } => None,
                    };
                    if let Some(reply) = reply {
                        if stream.write_all(&encode(&reply)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctrlvim_types::Object;

    fn request(msgid: u32, method: &str) -> Vec<u8> {
        encode(&Message::Request {
            msgid,
            method: method.to_string(),
            params: vec![Object::Integer(1)],
        })
    }

    #[test]
    fn a_whole_message_decodes_in_one_feed() {
        let mut conn = Connection::new();
        let msgs = conn.feed(&request(1, "ctrlvim_get_current_line")).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(conn.pending(), 0);
    }

    #[test]
    fn a_split_message_waits_for_the_rest() {
        // The case a length-prefixed protocol gets for free and this one does
        // not: a message arriving across two reads.
        let bytes = request(7, "ctrlvim_buf_line_count");
        let (head, tail) = bytes.split_at(bytes.len() / 2);
        let mut conn = Connection::new();
        assert!(conn.feed(head).unwrap().is_empty(), "incomplete yields nothing");
        assert!(conn.pending() > 0);
        let msgs = conn.feed(tail).unwrap();
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            Message::Request { msgid, method, .. } => {
                assert_eq!(*msgid, 7);
                assert_eq!(method, "ctrlvim_buf_line_count");
            }
            _ => panic!("expected a request"),
        }
        assert_eq!(conn.pending(), 0);
    }

    #[test]
    fn several_messages_in_one_read_all_decode() {
        let mut bytes = request(1, "a");
        bytes.extend(request(2, "b"));
        bytes.extend(request(3, "c"));
        let mut conn = Connection::new();
        let msgs = conn.feed(&bytes).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(conn.pending(), 0);
    }

    #[test]
    fn a_byte_at_a_time_still_works() {
        let bytes = request(9, "ctrlvim_get_current_buf");
        let mut conn = Connection::new();
        let mut got = Vec::new();
        for b in &bytes {
            got.extend(conn.feed(&[*b]).unwrap());
        }
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn garbage_is_reported_rather_than_looping() {
        let mut conn = Connection::new();
        // 0xc1 is the one marker msgpack leaves permanently unused.
        let err = conn.feed(&[0xc1, 0xc1, 0xc1]);
        assert!(err.is_err());
        assert_eq!(conn.pending(), 0, "the desynced buffer is dropped");
    }

    #[tokio::test]
    async fn a_client_can_attach_and_call() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let path = std::env::temp_dir().join(format!("ctrlvim-rpc-test-{}.sock", std::process::id()));
        let server = RpcServer::bind(&path).unwrap();
        let sock = server.path().to_path_buf();

        // The server runs until the test drops it.
        tokio::spawn(async move {
            let _ = server
                .serve(|method, _params| {
                    if method == "ping" {
                        Ok(Object::str("pong".to_string()))
                    } else {
                        Err(format!("unknown method: {method}"))
                    }
                })
                .await;
        });

        let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
        client.write_all(&request(42, "ping")).await.unwrap();

        let mut buf = [0u8; 1024];
        let n = client.read(&mut buf).await.unwrap();
        let reply = decode(&buf[..n]).unwrap();
        match reply {
            Message::Response { msgid, error, result } => {
                assert_eq!(msgid, 42);
                assert!(error.is_none(), "ping should succeed");
                assert_eq!(result.as_str(), Some("pong"));
            }
            _ => panic!("expected a response"),
        }
    }

    #[tokio::test]
    async fn an_unknown_method_comes_back_as_an_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let path =
            std::env::temp_dir().join(format!("ctrlvim-rpc-err-{}.sock", std::process::id()));
        let server = RpcServer::bind(&path).unwrap();
        let sock = server.path().to_path_buf();
        tokio::spawn(async move {
            let _ = server
                .serve(|method, _| Err(format!("unknown method: {method}")))
                .await;
        });

        let mut client = tokio::net::UnixStream::connect(&sock).await.unwrap();
        client.write_all(&request(1, "nope")).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = client.read(&mut buf).await.unwrap();
        match decode(&buf[..n]).unwrap() {
            Message::Response { error, .. } => {
                let msg = error.expect("expected an error");
                assert!(msg.as_str().unwrap().contains("unknown method"));
            }
            _ => panic!("expected a response"),
        }
    }
}
