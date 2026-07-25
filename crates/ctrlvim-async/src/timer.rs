//! Timer service — the tokio-backed replacement for libuv's `uv_timer_t`, the
//! substrate `vim.uv.new_timer()` is built on.
//!
//! Neovim registers timers on the libuv loop; callbacks fire on the loop thread
//! and are marshaled to the main thread. Here a tokio task sleeps and then pushes
//! a [`Event::TimerFired`] onto the [`EventLoop`] queue; the main thread invokes
//! the associated Lua callback when it drains the queue. This keeps callback
//! invocation single-threaded (the editor is not `Send`) while the waiting
//! happens on tokio.

use crate::event::Event;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

/// Owns the tokio runtime and hands out timers. One per editor.
pub struct TimerService {
    rt: Runtime,
    tx: Sender<Event>,
    next_id: u64,
}

/// A handle to a scheduled timer; dropping or calling [`TimerHandle::stop`]
/// cancels future firings.
pub struct TimerHandle {
    pub id: u64,
    cancelled: Arc<AtomicBool>,
}

impl TimerHandle {
    /// Stop the timer (`uv_timer_stop`).
    pub fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl TimerService {
    pub fn new(tx: Sender<Event>) -> std::io::Result<Self> {
        // IO as well as time: this runtime is shared with [`crate::job::Jobs`],
        // and tokio's child-process pipes panic without the IO driver.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .enable_io()
            .build()?;
        Ok(TimerService { rt, tx, next_id: 1 })
    }

    /// Schedule a timer that fires after `timeout` and then every `repeat`
    /// (0 = one-shot), pushing `TimerFired(id)` onto the event queue each time.
    pub fn start(&mut self, timeout: Duration, repeat: Duration) -> TimerHandle {
        let id = self.next_id;
        self.next_id += 1;
        let tx = self.tx.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_task = cancelled.clone();

        self.rt.spawn(async move {
            tokio::time::sleep(timeout).await;
            if cancel_task.load(Ordering::SeqCst) {
                return;
            }
            if tx.send(Event::TimerFired(id)).is_err() {
                return;
            }
            if repeat.is_zero() {
                return;
            }
            loop {
                tokio::time::sleep(repeat).await;
                if cancel_task.load(Ordering::SeqCst) {
                    return;
                }
                if tx.send(Event::TimerFired(id)).is_err() {
                    return;
                }
            }
        });

        TimerHandle { id, cancelled }
    }

    /// Access the runtime handle (for spawning process/RPC I/O tasks).
    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventLoop;

    #[test]
    fn oneshot_timer_fires_once() {
        let el = EventLoop::new();
        let mut svc = TimerService::new(el.sender()).unwrap();
        let _h = svc.start(Duration::from_millis(10), Duration::ZERO);
        let ev = el.wait(Duration::from_secs(1));
        assert_eq!(ev, Some(Event::TimerFired(1)));
        // No further events for a one-shot.
        assert!(el.wait(Duration::from_millis(30)).is_none());
    }

    #[test]
    fn repeating_timer_fires_multiple_times() {
        let el = EventLoop::new();
        let mut svc = TimerService::new(el.sender()).unwrap();
        let h = svc.start(Duration::from_millis(5), Duration::from_millis(5));
        let mut count = 0;
        for _ in 0..3 {
            if el.wait(Duration::from_secs(1)).is_some() {
                count += 1;
            }
        }
        h.stop();
        assert_eq!(count, 3);
    }

    #[test]
    fn cancelled_timer_does_not_fire() {
        let el = EventLoop::new();
        let mut svc = TimerService::new(el.sender()).unwrap();
        let h = svc.start(Duration::from_millis(50), Duration::ZERO);
        h.stop();
        assert!(el.wait(Duration::from_millis(120)).is_none());
    }
}
