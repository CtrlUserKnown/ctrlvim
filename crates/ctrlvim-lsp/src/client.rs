//! One language server's process lifecycle and the handful of LSP methods
//! completion needs — `initialize`/`initialized`, `textDocument/didOpen`/
//! `didChange`/`didClose`, and `textDocument/completion`.
//!
//! Deliberately **not** a general-purpose LSP client: no hover, no
//! diagnostics, no signature help, no `completionItem/resolve`, no
//! incremental sync (every `didChange` ships the whole document — simpler
//! and correct, just more bytes over a local pipe than a real editor would
//! send). If ctrlvim grows those features later they belong here, but
//! completion is all that was asked for.
//!
//! # Handshake ordering
//!
//! The spec says a client must not send requests/notifications before the
//! server has responded to `initialize` — so `did_open`/`did_change` calls
//! that arrive during that window are queued (`pending_sync`) and flushed,
//! in order, right after `initialized` goes out. `request_completion` is not
//! queued: it is transient and keystroke-driven (see the `seq` staleness
//! token), so a query that arrives before the server is ready is simply
//! dropped — the caller's buffer-word fallback covers that gap, and the next
//! keystroke asks again.

use std::collections::HashMap;
use std::path::Path;

use ctrlvim_async::{JobStdin, Jobs};
use serde_json::{json, Value};

use crate::codec::{encode, Decoder};

/// Where a client's handshake is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientStatus {
    Starting,
    Ready,
    /// The process exited or the handshake failed; `String` is why, for the
    /// status line. A failed client is inert — nothing is retried.
    Failed(String),
}

/// A completion candidate, projected down from LSP's `CompletionItem` to what
/// the popup actually shows and inserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    /// What to insert, snippet placeholders already stripped to plain text
    /// (see [`strip_snippet`]) — typing the completion never leaves stray
    /// `$1`/`${1:name}` markers behind.
    pub insert_text: String,
    /// A short LSP `CompletionItemKind` label ("Function", "Field", …), for
    /// the popup's dim suffix.
    pub kind: Option<&'static str>,
    pub detail: Option<String>,
}

/// Something a caller needs to react to, produced by [`LspClient::feed_stdout`].
#[derive(Debug, Clone, PartialEq)]
pub enum LspEvent {
    /// The handshake finished; the client will now accept document sync and
    /// completion requests.
    Ready,
    /// A reply to [`LspClient::request_completion`]. `seq` is exactly what
    /// was passed to that call — compare it against the latest request
    /// before trusting `items`, the same way `ctrlvim_ai::Reply` works.
    Completion { seq: u64, items: Vec<CompletionItem> },
    /// The process exited or the handshake failed.
    Failed(String),
}

/// Pending outbound requests, keyed by JSON-RPC id, so a response (which only
/// carries the id back) can be routed to what asked for it.
enum Pending {
    Initialize,
    Completion { seq: u64 },
}

pub struct LspClient {
    pub job_id: u64,
    stdin: JobStdin,
    decoder: Decoder,
    status: ClientStatus,
    next_request_id: i64,
    pending: HashMap<i64, Pending>,
    /// Encoded `didOpen`/`didChange` bytes queued while `status` is still
    /// `Starting` — see the module docs' "handshake ordering" note.
    pending_sync: Vec<Vec<u8>>,
    /// Document version per open URI (`textDocument/didChange` needs a
    /// monotonically increasing one; `didOpen` starts a document at 1).
    doc_versions: HashMap<String, i64>,
}

impl LspClient {
    /// Spawn `program` (a binary name or path the caller already resolved —
    /// e.g. from a `[[server]]` declaration in `lsp.lua`) as a language
    /// server rooted at `root`, and send `initialize` immediately.
    pub fn spawn(jobs: &mut Jobs, program: &str, args: &[String], root: &Path) -> Self {
        let (job_id, stdin) = jobs.spawn_persistent(program, args, root);
        let mut client = LspClient {
            job_id,
            stdin,
            decoder: Decoder::new(),
            status: ClientStatus::Starting,
            next_request_id: 1,
            pending: HashMap::new(),
            pending_sync: Vec::new(),
            doc_versions: HashMap::new(),
        };
        let id = client.next_id();
        client.pending.insert(id, Pending::Initialize);
        client.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": uri_from_path(root),
                "capabilities": {
                    "textDocument": {
                        "completion": {
                            "completionItem": { "snippetSupport": false }
                        }
                    }
                }
            }
        }));
        client
    }

    pub fn status(&self) -> &ClientStatus {
        &self.status
    }

    fn next_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    fn send(&self, msg: Value) {
        self.stdin.write(encode(&msg));
    }

    /// Queue a notification while not yet `Ready`, or send it immediately
    /// once past the handshake.
    fn send_or_queue(&mut self, msg: Value) {
        if self.status == ClientStatus::Ready {
            self.send(msg);
        } else {
            self.pending_sync.push(encode(&msg));
        }
    }

    fn flush_pending_sync(&mut self) {
        for bytes in self.pending_sync.drain(..) {
            self.stdin.write(bytes);
        }
    }

    /// Open a document, or resync it if this client already thinks it's
    /// open (a buffer can be re-`did_open`ed after being closed and
    /// reopened; treated as a fresh version-1 open either way).
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) {
        self.doc_versions.insert(uri.to_string(), 1);
        self.send_or_queue(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }
        }));
    }

    /// Full-document sync: ships the whole new text under the next version
    /// number. If this URI was never opened, opens it instead — a
    /// `didChange` on an unopened document is meaningless to a server.
    pub fn did_change(&mut self, uri: &str, language_id: &str, text: &str) {
        let Some(version) = self.doc_versions.get_mut(uri) else {
            return self.did_open(uri, language_id, text);
        };
        *version += 1;
        let version = *version;
        self.send_or_queue(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            }
        }));
    }

    pub fn did_close(&mut self, uri: &str) {
        if self.doc_versions.remove(uri).is_none() {
            return;
        }
        self.send_or_queue(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } }
        }));
    }

    /// Ask for completions at `(line, character)` (0-based, LSP's own
    /// convention). `seq` comes back unchanged on the `LspEvent::Completion`
    /// this produces, for the caller to check staleness. A no-op — not
    /// queued — before the handshake finishes; see the module docs.
    pub fn request_completion(&mut self, uri: &str, line: usize, character: usize, seq: u64) {
        if self.status != ClientStatus::Ready {
            return;
        }
        let id = self.next_id();
        self.pending.insert(id, Pending::Completion { seq });
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }
        }));
    }

    /// Best-effort polite shutdown — most servers exit on their own once
    /// they see `exit`. Not waited on; the process finishing is just another
    /// `ProcessExit` the caller can ignore once it already knows it asked
    /// for this.
    pub fn shutdown(&mut self) {
        let id = self.next_id();
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown", "params": null }));
        self.send(json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
    }

    /// Feed newly-arrived stdout bytes, returning whatever the caller now
    /// needs to know about.
    pub fn feed_stdout(&mut self, data: &[u8]) -> Vec<LspEvent> {
        let messages = self.decoder.feed(data);
        let mut out = Vec::new();
        for msg in messages {
            if let Some(event) = self.handle_message(msg) {
                out.push(event);
            }
        }
        out
    }

    /// Stderr is a server's own logging, not protocol — nothing here parses
    /// it. It only matters if the process is dying, and `handle_exit` is
    /// what reports that.
    pub fn feed_stderr(&mut self, _data: &[u8]) {}

    /// The process exited. Always `Failed` — a client this crate spawned is
    /// only ever torn down through [`shutdown`](Self::shutdown), and a
    /// caller that just called that already knows why, so there's no
    /// "expected exit" event to distinguish here.
    pub fn handle_exit(&mut self, code: i64) -> LspEvent {
        let reason = format!("server exited (code {code})");
        self.status = ClientStatus::Failed(reason.clone());
        LspEvent::Failed(reason)
    }

    fn handle_message(&mut self, msg: Value) -> Option<LspEvent> {
        let id = msg.get("id").and_then(Value::as_i64);
        let has_method = msg.get("method").is_some();

        // A request *from* the server (has both `id` and `method`) — this
        // client implements none of them, but many servers (rust-analyzer
        // included) block on `client/registerCapability` and similar until
        // they see *some* response, so an empty ack keeps things moving.
        if has_method && id.is_some() {
            self.send(json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null }));
            return None;
        }
        // A notification (has `method`, no `id`) — `publishDiagnostics`,
        // `window/logMessage`, `$/progress`, none of which this client acts
        // on.
        if has_method {
            return None;
        }
        // Otherwise it's a response to one of ours.
        let id = id?;
        match self.pending.remove(&id)? {
            Pending::Initialize => {
                self.status = ClientStatus::Ready;
                self.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
                self.flush_pending_sync();
                Some(LspEvent::Ready)
            }
            Pending::Completion { seq } => {
                let items = msg.get("result").map(parse_completion_result).unwrap_or_default();
                Some(LspEvent::Completion { seq, items })
            }
        }
    }
}

/// `result` of `textDocument/completion` is `null`, a bare `CompletionItem[]`,
/// or `{ isIncomplete, items }` — all three are valid per spec.
fn parse_completion_result(result: &Value) -> Vec<CompletionItem> {
    let items = if let Some(items) = result.get("items") {
        items.as_array()
    } else {
        result.as_array()
    };
    items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_string();
            let raw_insert = item
                .get("textEdit")
                .and_then(|e| e.get("newText"))
                .or_else(|| item.get("insertText"))
                .and_then(Value::as_str)
                .unwrap_or(&label);
            let insert_text = strip_snippet(raw_insert);
            let kind = item.get("kind").and_then(Value::as_i64).and_then(kind_label);
            let detail = item.get("detail").and_then(Value::as_str).map(str::to_string);
            Some(CompletionItem { label, insert_text, kind, detail })
        })
        .collect()
}

/// Degrade an LSP snippet (`insertTextFormat == 2`) to plain text: `$0`/`$1`
/// placeholders are dropped, `${1:default}` keeps `default`. Good enough to
/// never insert garbage; not a snippet engine — no tabstop navigation.
fn strip_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next(); // consume '{'
                let mut depth = 1;
                let mut inner = String::new();
                for c in chars.by_ref() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    inner.push(c);
                }
                // `${N:default}` keeps the text after the first `:`;
                // `${N}` (no default) contributes nothing.
                if let Some((_, default)) = inner.split_once(':') {
                    out.push_str(default);
                }
            }
            Some(d) if d.is_ascii_digit() => {
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                    chars.next();
                }
            }
            _ => out.push(c), // a bare trailing '$'
        }
    }
    out
}

/// `CompletionItemKind` values completion popups commonly show; anything
/// else (there are ~25 in the spec) just gets no kind suffix rather than a
/// number nobody can read.
fn kind_label(kind: i64) -> Option<&'static str> {
    Some(match kind {
        2 => "Method",
        3 => "Function",
        4 => "Constructor",
        5 => "Field",
        6 => "Variable",
        7 => "Class",
        8 => "Interface",
        9 => "Module",
        10 => "Property",
        13 => "Enum",
        14 => "Keyword",
        20 => "EnumMember",
        21 => "Constant",
        22 => "Struct",
        _ => return None,
    })
}

/// `file://` + the path, with the one character (space) common enough in
/// real project paths to matter percent-encoded. Not a general URI encoder —
/// exotic path characters are a known gap.
pub fn uri_from_path(path: &Path) -> String {
    format!("file://{}", path.display().to_string().replace(' ', "%20"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_snippet_drops_tabstops_and_keeps_defaults() {
        assert_eq!(strip_snippet("println!(\"$1\")$0"), "println!(\"\")");
        assert_eq!(strip_snippet("fn ${1:name}(${2:args})"), "fn name(args)");
        assert_eq!(strip_snippet("plain_text"), "plain_text");
        assert_eq!(strip_snippet("price: $5"), "price: ");
    }

    #[test]
    fn parse_completion_result_reads_a_bare_array() {
        let result = serde_json::json!([
            {"label": "foo", "kind": 3, "detail": "fn foo()"},
            {"label": "bar", "insertText": "bar_snippet($1)", "kind": 2},
        ]);
        let items = parse_completion_result(&result);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "foo");
        assert_eq!(items[0].kind, Some("Function"));
        assert_eq!(items[0].detail.as_deref(), Some("fn foo()"));
        assert_eq!(items[1].insert_text, "bar_snippet()");
        assert_eq!(items[1].kind, Some("Method"));
    }

    #[test]
    fn parse_completion_result_reads_a_completion_list() {
        let result = serde_json::json!({"isIncomplete": false, "items": [{"label": "x"}]});
        assert_eq!(parse_completion_result(&result), vec![CompletionItem {
            label: "x".into(),
            insert_text: "x".into(),
            kind: None,
            detail: None,
        }]);
    }

    #[test]
    fn text_edit_wins_over_plain_insert_text() {
        let result = serde_json::json!([{
            "label": "foo",
            "insertText": "wrong",
            "textEdit": { "newText": "right", "range": {} },
        }]);
        assert_eq!(parse_completion_result(&result)[0].insert_text, "right");
    }

    #[test]
    fn uri_from_path_encodes_spaces() {
        assert_eq!(uri_from_path(Path::new("/a b/c")), "file:///a%20b/c");
    }
}
