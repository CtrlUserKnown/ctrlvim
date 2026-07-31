//! Job control — spawning external programs, the substrate under `:make`,
//! `:grep`, `jobstart()`, and (later) an LSP server's stdio channel.
//!
//! Same shape as [`crate::timer`]: tokio tasks do the waiting and push
//! [`Event`]s onto the queue, so the main thread stays single-threaded and
//! never blocks. Output arrives as [`Event::ProcessOutput`] chunks (line
//! boundaries are *not* guaranteed — the consumer reassembles) followed by
//! exactly one [`Event::ProcessExit`].
//!
//! stdout and stderr are merged deliberately: compilers put diagnostics on
//! stderr and progress on stdout, and the quickfix list wants both in order.

use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Sender;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::event::Event;

/// The writable half of a [`Jobs::spawn_persistent`] child's stdin. Bytes
/// queued with [`JobStdin::write`] are written in order on the job's own
/// tokio task; dropping (or [`JobStdin::close`]ing) the last handle closes
/// the child's stdin, which is how a well-behaved LSP server (or any process
/// that reads until EOF) learns the conversation is over.
#[derive(Clone)]
pub struct JobStdin {
    tx: UnboundedSender<Vec<u8>>,
}

impl JobStdin {
    /// Queue `data` to be written to the child's stdin. Silently dropped if
    /// the process already exited — a write racing a server's own shutdown
    /// is not an error the caller needs to handle.
    pub fn write(&self, data: Vec<u8>) {
        let _ = self.tx.send(data);
    }

    /// Explicitly close stdin now, rather than waiting for every clone to
    /// drop — the caller-facing spelling of "I'm done sending".
    pub fn close(self) {}
}

/// Spawns programs onto a tokio runtime, tagging each with an id the caller
/// uses to match output and exit events.
pub struct Jobs {
    handle: Handle,
    tx: Sender<Event>,
    next_id: u64,
}

impl Jobs {
    /// Build a job service on an existing runtime — share
    /// [`crate::timer::TimerService::runtime`] rather than starting a second one.
    pub fn new(handle: Handle, tx: Sender<Event>) -> Self {
        Jobs { handle, tx, next_id: 1 }
    }

    /// Spawn `program` with `args` in `cwd`, streaming its merged output.
    ///
    /// Returns the job id immediately; the process runs in the background. A
    /// program that can't be started still produces a `ProcessExit`, with the
    /// conventional shell code 127, so a caller waiting on completion never
    /// hangs on a typo'd command.
    pub fn spawn(&mut self, program: &str, args: &[String], cwd: &Path) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let tx = self.tx.clone();
        // Owned, since the spawned task outlives this call.
        let program = program.to_string();
        let mut command = Command::new(&program);
        command
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        self.handle.spawn(async move {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(Event::ProcessOutput {
                        id,
                        data: format!("{program}: {e}\n").into_bytes(),
                    });
                    let _ = tx.send(Event::ProcessExit { id, code: 127 });
                    return;
                }
            };

            // Read both pipes concurrently; interleaving is fine because the
            // consumer reassembles lines.
            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();
            let out_tx = tx.clone();
            let err_tx = tx.clone();
            let pump_out = async move {
                if let Some(pipe) = stdout.as_mut() {
                    pump(pipe, id, &out_tx, |id, data| Event::ProcessOutput { id, data }).await;
                }
            };
            let pump_err = async move {
                if let Some(pipe) = stderr.as_mut() {
                    pump(pipe, id, &err_tx, |id, data| Event::ProcessOutput { id, data }).await;
                }
            };
            let status = tokio::join!(pump_out, pump_err, child.wait()).2;

            let code = match status {
                Ok(status) => status.code().unwrap_or(-1) as i64,
                Err(_) => -1,
            };
            let _ = tx.send(Event::ProcessExit { id, code });
        });

        id
    }

    /// Spawn `command` through `shell -c`, for callers holding a raw command
    /// line (`:!{cmd}`) rather than a pre-split program and argument list.
    pub fn spawn_shell(&mut self, shell: &str, command: &str, cwd: &Path) -> u64 {
        self.spawn(shell, &["-c".to_string(), command.to_string()], cwd)
    }

    /// Spawn a long-lived process with a writable stdin and *separate*
    /// stdout/stderr streams (`Event::ProcessStdout`/`ProcessStderr`) — what
    /// an LSP server (framed JSON-RPC on stdout, free-form logs on stderr) or
    /// a stdin-fed formatter needs, and what [`Jobs::spawn`] deliberately
    /// doesn't provide (merged output, no stdin).
    ///
    /// Returns immediately with the job id and a [`JobStdin`] to write to;
    /// the process runs in the background exactly like [`Jobs::spawn`].
    pub fn spawn_persistent(&mut self, program: &str, args: &[String], cwd: &Path) -> (u64, JobStdin) {
        let id = self.next_id;
        self.next_id += 1;

        let tx = self.tx.clone();
        let program_owned = program.to_string();
        let mut command = Command::new(&program_owned);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let (stdin_tx, mut stdin_rx) = unbounded_channel::<Vec<u8>>();

        self.handle.spawn(async move {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(Event::ProcessStderr {
                        id,
                        data: format!("{program_owned}: {e}\n").into_bytes(),
                    });
                    let _ = tx.send(Event::ProcessExit { id, code: 127 });
                    return;
                }
            };

            let mut stdin = child.stdin.take();
            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();

            let write_stdin = async move {
                if let Some(pipe) = stdin.as_mut() {
                    while let Some(chunk) = stdin_rx.recv().await {
                        if pipe.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    // Every sender (the `JobStdin` the caller holds, plus any
                    // clones) is gone, or a write failed — either way, close
                    // stdin so a process reading until EOF can proceed.
                    let _ = pipe.shutdown().await;
                }
            };
            let out_tx = tx.clone();
            let pump_out = async move {
                if let Some(pipe) = stdout.as_mut() {
                    pump(pipe, id, &out_tx, |id, data| Event::ProcessStdout { id, data }).await;
                }
            };
            let err_tx = tx.clone();
            let pump_err = async move {
                if let Some(pipe) = stderr.as_mut() {
                    pump(pipe, id, &err_tx, |id, data| Event::ProcessStderr { id, data }).await;
                }
            };
            let status = tokio::join!(write_stdin, pump_out, pump_err, child.wait()).3;

            let code = match status {
                Ok(status) => status.code().unwrap_or(-1) as i64,
                Err(_) => -1,
            };
            let _ = tx.send(Event::ProcessExit { id, code });
        });

        (id, JobStdin { tx: stdin_tx })
    }
}

/// Forward everything a pipe produces onto the event queue, wrapped by `mk`
/// into whichever `Event` variant the caller's stream should become
/// (`ProcessOutput` for a merged stream, `ProcessStdout`/`ProcessStderr` for
/// a [`Jobs::spawn_persistent`] job's separate ones).
async fn pump<R>(pipe: &mut R, id: u64, tx: &Sender<Event>, mk: impl Fn(u64, Vec<u8>) -> Event)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(mk(id, buf[..n].to_vec())).is_err() {
                    // The editor is gone; stop reading.
                    return;
                }
            }
        }
    }
}

/// Reassembles [`Event::ProcessOutput`] chunks into whole lines.
///
/// Chunks split wherever the pipe buffer happened to fill, so a consumer that
/// parses per line (the quickfix list, an LSP header) needs this in between.
#[derive(Default)]
pub struct LineBuffer {
    partial: String,
}

impl LineBuffer {
    pub fn new() -> Self {
        LineBuffer::default()
    }

    /// Feed a chunk, returning every *complete* line it finished. Invalid UTF-8
    /// is replaced rather than dropped, so a binary-ish build log can't stall
    /// the stream.
    pub fn push(&mut self, data: &[u8]) -> Vec<String> {
        self.partial.push_str(&String::from_utf8_lossy(data));
        let mut lines = Vec::new();
        while let Some(i) = self.partial.find('\n') {
            let line = self.partial[..i].trim_end_matches('\r').to_string();
            self.partial.drain(..=i);
            lines.push(line);
        }
        lines
    }

    /// Take whatever is left when the process exits without a trailing newline.
    pub fn flush(&mut self) -> Option<String> {
        (!self.partial.is_empty()).then(|| std::mem::take(&mut self.partial))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventLoop;
    use crate::timer::TimerService;
    use std::time::Duration;

    /// Drain events until the job exits, returning (output, exit code).
    fn run(program: &str, args: &[&str]) -> (String, i64) {
        let el = EventLoop::new();
        let svc = TimerService::new(el.sender()).unwrap();
        let mut jobs = Jobs::new(svc.runtime().handle().clone(), el.sender());
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        jobs.spawn(program, &args, Path::new("."));

        let mut out = String::new();
        loop {
            match el.wait(Duration::from_secs(10)) {
                Some(Event::ProcessOutput { data, .. }) => {
                    out.push_str(&String::from_utf8_lossy(&data))
                }
                Some(Event::ProcessExit { code, .. }) => return (out, code),
                _ => panic!("job produced no exit event"),
            }
        }
    }

    #[test]
    fn captures_stdout_and_the_exit_code() {
        let (out, code) = run("echo", &["hello"]);
        assert_eq!(out.trim(), "hello");
        assert_eq!(code, 0);
    }

    #[test]
    fn captures_stderr_and_a_failing_status() {
        let (out, code) = run("sh", &["-c", "echo oops >&2; exit 3"]);
        assert_eq!(out.trim(), "oops", "stderr is merged into the same stream");
        assert_eq!(code, 3);
    }

    #[test]
    fn spawn_shell_runs_a_raw_command_line() {
        let el = EventLoop::new();
        let svc = TimerService::new(el.sender()).unwrap();
        let mut jobs = Jobs::new(svc.runtime().handle().clone(), el.sender());
        jobs.spawn_shell("sh", "echo a && echo b", Path::new("."));

        let mut out = String::new();
        loop {
            match el.wait(Duration::from_secs(10)) {
                Some(Event::ProcessOutput { data, .. }) => {
                    out.push_str(&String::from_utf8_lossy(&data))
                }
                Some(Event::ProcessExit { code, .. }) => {
                    assert_eq!(code, 0);
                    break;
                }
                _ => panic!("job produced no exit event"),
            }
        }
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn a_missing_program_still_exits() {
        // No hang, and the error is reported as output rather than swallowed.
        let (out, code) = run("ctrlvim-definitely-not-a-program", &[]);
        assert_eq!(code, 127);
        assert!(out.contains("ctrlvim-definitely-not-a-program"));
    }

    #[test]
    fn spawn_persistent_writes_to_stdin_and_reads_stdout_separately_from_stderr() {
        let el = EventLoop::new();
        let svc = TimerService::new(el.sender()).unwrap();
        let mut jobs = Jobs::new(svc.runtime().handle().clone(), el.sender());
        let (_id, stdin) =
            jobs.spawn_persistent("sh", &["-c".to_string(), "cat; echo oops >&2".to_string()], Path::new("."));
        stdin.write(b"hello ".to_vec());
        stdin.write(b"world".to_vec());
        stdin.close();

        let mut out = String::new();
        let mut err = String::new();
        loop {
            match el.wait(Duration::from_secs(10)) {
                Some(Event::ProcessStdout { data, .. }) => out.push_str(&String::from_utf8_lossy(&data)),
                Some(Event::ProcessStderr { data, .. }) => err.push_str(&String::from_utf8_lossy(&data)),
                Some(Event::ProcessExit { code, .. }) => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(out, "hello world", "cat echoes exactly what stdin sent, nothing from stderr mixed in");
        assert_eq!(err.trim(), "oops", "stderr arrives on its own event, not merged into stdout");
    }

    #[test]
    fn spawn_persistent_closing_stdin_lets_a_read_until_eof_process_exit() {
        let el = EventLoop::new();
        let svc = TimerService::new(el.sender()).unwrap();
        let mut jobs = Jobs::new(svc.runtime().handle().clone(), el.sender());
        let (_id, stdin) = jobs.spawn_persistent("cat", &[], Path::new("."));
        // No writes at all — closing stdin immediately should still let `cat`
        // see EOF and exit cleanly rather than hang forever.
        stdin.close();

        match el.wait(Duration::from_secs(10)) {
            Some(Event::ProcessExit { code, .. }) => assert_eq!(code, 0),
            other => panic!("expected a prompt exit, got {other:?}"),
        }
    }

    #[test]
    fn spawn_persistent_a_missing_program_still_exits() {
        let el = EventLoop::new();
        let svc = TimerService::new(el.sender()).unwrap();
        let mut jobs = Jobs::new(svc.runtime().handle().clone(), el.sender());
        let (_id, _stdin) = jobs.spawn_persistent("ctrlvim-definitely-not-a-program", &[], Path::new("."));

        let mut err = String::new();
        loop {
            match el.wait(Duration::from_secs(10)) {
                Some(Event::ProcessStderr { data, .. }) => err.push_str(&String::from_utf8_lossy(&data)),
                Some(Event::ProcessExit { code, .. }) => {
                    assert_eq!(code, 127);
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(err.contains("ctrlvim-definitely-not-a-program"), "{err}");
    }

    #[test]
    fn line_buffer_reassembles_split_chunks() {
        let mut lb = LineBuffer::new();
        assert!(lb.push(b"src/a.rs:1").is_empty(), "no newline yet, nothing complete");
        assert_eq!(lb.push(b":5: error\nsrc/b"), vec!["src/a.rs:1:5: error"]);
        assert_eq!(lb.push(b".rs:2:1: warn\n"), vec!["src/b.rs:2:1: warn"]);
        assert_eq!(lb.flush(), None, "nothing left over");
    }

    #[test]
    fn line_buffer_handles_crlf_and_a_missing_final_newline() {
        let mut lb = LineBuffer::new();
        assert_eq!(lb.push(b"one\r\ntwo"), vec!["one"]);
        assert_eq!(lb.flush(), Some("two".to_string()));
        assert_eq!(lb.flush(), None, "flush consumes the remainder");
    }
}
