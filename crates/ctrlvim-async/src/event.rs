//! The event queue — the replacement for `event/multiqueue.c`.
//!
//! Neovim marshals events from libuv callbacks (which fire on the loop thread)
//! onto the main thread's serial processing point via a "multiqueue". We use a
//! plain `mpsc` channel: background tokio tasks (timers, RPC readers, job I/O)
//! push [`Event`]s, and the main editor loop drains them between keystrokes —
//! exactly the `K_EVENT` mechanism, minus the callback-registration model.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// An event delivered to the main loop from a background source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A timer with this id elapsed.
    TimerFired(u64),
    /// An incoming RPC request/notification (raw msgpack bytes).
    RpcMessage(Vec<u8>),
    /// A spawned process wrote to stdout or stderr — merged into one stream
    /// (`Jobs::spawn`/`spawn_shell`), which is what a compiler/`:make`-style
    /// consumer wants (diagnostics on either stream, in order).
    ProcessOutput { id: u64, data: Vec<u8> },
    /// A persistent process's stdout, kept separate from stderr
    /// (`Jobs::spawn_persistent`) — required for anything that frames a
    /// protocol over stdout (LSP's `Content-Length`-prefixed JSON-RPC): a
    /// server's own stderr logging must never be able to corrupt the stream.
    ProcessStdout { id: u64, data: Vec<u8> },
    /// A persistent process's stderr, kept separate from stdout for the same
    /// reason (`Jobs::spawn_persistent`).
    ProcessStderr { id: u64, data: Vec<u8> },
    /// A spawned process exited (either spawn path).
    ProcessExit { id: u64, code: i64 },
}

/// The main-thread end of the event queue.
pub struct EventLoop {
    rx: Receiver<Event>,
    tx: Sender<Event>,
}

impl EventLoop {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        EventLoop { rx, tx }
    }

    /// A cloneable sender handed to background tasks.
    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    /// Drain all currently-ready events without blocking. This is what the main
    /// loop calls each iteration to process pending `K_EVENT`s.
    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Block up to `timeout` for the next event (the `input_get` wait).
    pub fn wait(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Default for EventLoop {
    fn default() -> Self {
        EventLoop::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_collects_pushed_events() {
        let el = EventLoop::new();
        let tx = el.sender();
        tx.send(Event::TimerFired(1)).unwrap();
        tx.send(Event::TimerFired(2)).unwrap();
        let events = el.drain();
        assert_eq!(events, vec![Event::TimerFired(1), Event::TimerFired(2)]);
        assert!(el.drain().is_empty());
    }

    #[test]
    fn wait_times_out() {
        let el = EventLoop::new();
        assert!(el.wait(Duration::from_millis(5)).is_none());
    }
}



