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

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::runtime::Handle;

use crate::event::Event;

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
                    pump(pipe, id, &out_tx).await;
                }
            };
            let pump_err = async move {
                if let Some(pipe) = stderr.as_mut() {
                    pump(pipe, id, &err_tx).await;
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
}

/// Forward everything a pipe produces onto the event queue.
async fn pump<R>(pipe: &mut R, id: u64, tx: &Sender<Event>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(Event::ProcessOutput { id, data: buf[..n].to_vec() }).is_err() {
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
    fn a_missing_program_still_exits() {
        // No hang, and the error is reported as output rather than swallowed.
        let (out, code) = run("ctrlvim-definitely-not-a-program", &[]);
        assert_eq!(code, 127);
        assert!(out.contains("ctrlvim-definitely-not-a-program"));
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
