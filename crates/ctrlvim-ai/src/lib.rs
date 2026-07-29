//! Local code completion for ctrlvim: CodeGemma-2B fill-in-the-middle, running
//! on [candle](https://github.com/huggingface/candle) in-process.
//!
//! This is the *host* half of inline suggestions. The engine
//! (`ctrlvim_editor::suggest`) owns what is being suggested and when it goes
//! stale; this crate owns the model, its weights, and the thread they run on,
//! and knows nothing about buffers or cursors.
//!
//! # Shape
//!
//! [`Completer`] is a handle to one background thread. The editor calls
//! [`submit`](Completer::submit) from its main loop and [`poll`](Completer::poll)
//! on the next frame — neither ever blocks, because a completion takes seconds
//! and a keystroke takes microseconds.
//!
//! Only the newest request matters. Superseding one flips the shared
//! "generation" counter, which the worker checks between tokens and which makes
//! an obsolete generation abandon its remaining budget instead of finishing a
//! completion nobody will see. Requests that pile up behind a slow generation
//! are collapsed to the last one for the same reason.
//!
//! # Cost
//!
//! CodeGemma-2B is a real 2.5-billion-parameter model: **~5GB** of weights at
//! bf16, downloaded once, and on a CPU it produces a few tokens per second.
//! That is why [`AiConfig`] leads with budgets — context window, token count,
//! wall-clock deadline — and why the feature ships off by default.
//!
//! The 1.6GB figure attached to this model elsewhere (Ollama's `codegemma:2b`
//! tag, the `*-GGUF` repos) is a 4-bit quantization, which is deliberately
//! *not* what this loads: candle-transformers has no quantized Gemma-1, only
//! quantized Gemma-3, and CodeGemma is Gemma-1. See `docs/ai.md`.

pub mod config;
pub mod prompt;

#[cfg(feature = "local-model")]
mod model;
#[cfg(feature = "local-model")]
mod quantized_gemma;

pub use config::{gated_repo_help, AiConfig, DevicePref, GgufSource, ModelSource, Precision};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

/// Where the model is. Cheap to clone and read every frame — the status line
/// does exactly that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing loaded and nothing asked for yet.
    Cold,
    /// Downloading or loading weights, with the current stage.
    Loading(String),
    /// Loaded and idle. Carries what was loaded, for `:AIStatus`.
    Ready(String),
    /// Generating a completion.
    Busy,
    /// The last load or generation failed.
    Failed(String),
}

impl Status {
    /// A one-line description for the status line / `:AIStatus`.
    pub fn describe(&self) -> String {
        match self {
            Status::Cold => "AI: not loaded".to_string(),
            Status::Loading(stage) => format!("AI: {stage}…"),
            Status::Ready(what) => format!("AI: ready — {what}"),
            Status::Busy => "AI: thinking…".to_string(),
            Status::Failed(e) => format!("AI: {e}"),
        }
    }

    /// A short marker for the status line, or `None` when there is nothing
    /// worth taking space for.
    pub fn badge(&self) -> Option<&'static str> {
        match self {
            Status::Cold => None,
            Status::Loading(_) => Some("AI ↓"),
            Status::Ready(_) => Some("AI"),
            Status::Busy => Some("AI …"),
            Status::Failed(_) => Some("AI !"),
        }
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Status::Failed(_))
    }
}

/// One completion to produce. `seq` is the engine's staleness token and is
/// echoed back untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub seq: u64,
    pub prefix: String,
    pub suffix: String,
    /// The buffer's name, when it has one. Included in the prompt because a
    /// filename is a strong language hint for a model trained on whole files.
    pub filename: Option<String>,
}

/// The answer to a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub seq: u64,
    /// The completion text (possibly empty), or why there isn't one.
    pub result: Result<String, String>,
}

/// What the worker thread is told to do.
enum Job {
    Load,
    Complete(Request),
    Stop,
}

/// A handle to the background completion worker.
pub struct Completer {
    jobs: Sender<Job>,
    replies: Receiver<Reply>,
    status: Arc<Mutex<Status>>,
    /// Bumped on every submission. The worker compares it against the request
    /// it started, and stops when they diverge.
    generation: Arc<AtomicU64>,
    config: AiConfig,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Completer {
    /// Spawn the worker. Cheap: no weights are touched until the first
    /// [`submit`](Self::submit) or [`preload`](Self::preload).
    pub fn new(config: AiConfig) -> Self {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel::<Reply>();
        let status = Arc::new(Mutex::new(Status::Cold));
        let generation = Arc::new(AtomicU64::new(0));

        let worker = {
            let config = config.clone();
            let status = Arc::clone(&status);
            let generation = Arc::clone(&generation);
            std::thread::Builder::new()
                .name("ctrlvim-ai".to_string())
                // The default 2MB stack is enough for candle's own recursion,
                // but model construction nests deeply through VarBuilder; give
                // it room rather than debugging a stack overflow later.
                .stack_size(16 * 1024 * 1024)
                .spawn(move || run_worker(config, job_rx, reply_tx, status, generation))
                .ok()
        };

        Completer { jobs: job_tx, replies: reply_rx, status, generation, config, worker }
    }

    /// The config this completer was built with.
    pub fn config(&self) -> &AiConfig {
        &self.config
    }

    /// Current model status.
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status::Cold)
    }

    /// Start loading the weights now, so the first suggestion isn't also the
    /// first (multi-gigabyte) download.
    pub fn preload(&self) {
        let _ = self.jobs.send(Job::Load);
    }

    /// Queue a completion, superseding any request still in flight.
    pub fn submit(&self, request: Request) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.send(Job::Complete(request));
    }

    /// Take the next finished reply, if one is waiting. Never blocks.
    pub fn poll(&self) -> Option<Reply> {
        match self.replies.try_recv() {
            Ok(reply) => Some(reply),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }
}

impl Drop for Completer {
    fn drop(&mut self) {
        // Wake the worker out of `recv` and let an in-flight generation see a
        // changed generation counter, so a `:AI off` mid-completion doesn't
        // wait out the token budget.
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.send(Job::Stop);
        // Deliberately *not* joined. The worker unwinds on its own once the
        // channels close, and joining would block the editor for as long as
        // whatever it is doing takes — which, on a cold cache, is a multi-
        // gigabyte download with no cancellation point. Turning suggestions off
        // must not freeze the UI until the weights finish arriving.
        drop(self.worker.take());
    }
}

fn set_status(cell: &Mutex<Status>, status: Status) {
    if let Ok(mut slot) = cell.lock() {
        *slot = status;
    }
}

/// Collapse a run of queued jobs down to the last completion request, so a
/// worker that fell behind while generating doesn't then work through a backlog
/// of contexts the user has already typed past.
fn latest(first: Job, rx: &Receiver<Job>) -> Job {
    let mut job = first;
    loop {
        match rx.try_recv() {
            // A stop wins outright, whatever came after it.
            Ok(Job::Stop) => return Job::Stop,
            // A load request is subsumed by a completion, which loads anyway.
            Ok(Job::Load) if matches!(job, Job::Complete(_)) => {}
            Ok(next) => job = next,
            Err(_) => return job,
        }
    }
}

/// Build the prompt halves for a request, bounded by the config's character
/// caps.
///
/// [`Request::filename`] is deliberately *not* spliced into the prefix. It
/// would be a useful language hint, but the only place to put it is as a line
/// of the code itself, and a bare filename is not valid in any of the languages
/// this completes — a guess at CodeGemma's repo-level layout that turns out
/// wrong costs more than the hint is worth.
///
/// Only the real worker calls this, so it is gated with it — otherwise the
/// default (backend-less) build warns about it on every compile. Its tests run
/// either way: what it does to a prompt is worth checking without a model.
#[cfg(any(feature = "local-model", test))]
fn prompt_parts(req: &Request, cfg: &AiConfig) -> (String, String) {
    let prefix = config::keep_tail(&req.prefix, cfg.max_prefix_chars);
    let suffix = config::keep_head(&req.suffix, cfg.max_suffix_chars);
    (prefix.to_string(), suffix.to_string())
}

#[cfg(feature = "local-model")]
fn run_worker(
    config: AiConfig,
    jobs: Receiver<Job>,
    replies: Sender<Reply>,
    status: Arc<Mutex<Status>>,
    generation: Arc<AtomicU64>,
) {
    let mut loaded: Option<model::CodeGemma> = None;

    // A bounded thread pool for candle to parallelize inside.
    //
    // Without this, candle uses rayon's *global* pool — every core on the
    // machine — and a 2B model keeps them all saturated for seconds per
    // completion. The editor's own thread then competes for CPU with the model,
    // and the whole application feels sluggish rather than just the
    // suggestions being slow. `install` below makes every nested rayon call
    // land in this pool instead.
    let threads = config::resolve_threads(
        config.threads,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
    );
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("ctrlvim-ai-{i}"))
        .build()
        .ok();

    while let Ok(job) = jobs.recv() {
        let job = latest(job, &jobs);
        let request = match job {
            Job::Stop => return,
            Job::Load => None,
            Job::Complete(req) => Some(req),
        };

        // The generation this job belongs to. Anything submitted after this
        // point makes it obsolete.
        let mine = generation.load(Ordering::SeqCst);
        let keep_going = || generation.load(Ordering::SeqCst) == mine;

        if loaded.is_none() {
            let progress = |stage: &str| set_status(&status, Status::Loading(stage.to_string()));
            match model::CodeGemma::load(&config.model, config.device, progress) {
                Ok(m) => {
                    set_status(&status, Status::Ready(m.description.clone()));
                    loaded = Some(m);
                }
                Err(e) => {
                    set_status(&status, Status::Failed(e.clone()));
                    if let Some(req) = &request {
                        let _ = replies.send(Reply { seq: req.seq, result: Err(e) });
                    }
                    // Don't retry on every keystroke: a gated repo or a missing
                    // file will fail identically until the user fixes it, and
                    // re-attempting the download each time makes the editor
                    // unusable. `:AILoad` clears this by asking again
                    // explicitly.
                    match wait_for_reload(&jobs) {
                        true => continue,
                        false => return,
                    }
                }
            }
        }

        let Some(req) = request else { continue };
        let Some(gemma) = loaded.as_mut() else { continue };
        if !keep_going() {
            continue;
        }

        set_status(&status, Status::Busy);
        let (prefix, suffix) = prompt_parts(&req, &config);
        let mut generate = || gemma.complete(&prefix, &suffix, &config, &keep_going);
        let result = match &pool {
            Some(pool) => pool.install(&mut generate),
            // A pool that failed to build is not worth failing the completion
            // over; it just means candle falls back to the global one.
            None => generate(),
        }
        .map(|raw| prompt::clean(&raw, &suffix, config.trim()));
        match &result {
            Ok(_) => {
                let what = gemma.description.clone();
                set_status(&status, Status::Ready(what));
            }
            Err(e) => set_status(&status, Status::Failed(e.clone())),
        }
        // A superseded generation's output is discarded rather than sent: the
        // engine would drop it on the seq check anyway, and not sending it
        // keeps that check from being load-bearing.
        if keep_going() {
            let _ = replies.send(Reply { seq: req.seq, result });
        }
    }
}

/// After a failed load, block until an explicit `:AILoad` (or a shutdown)
/// arrives, dropping completion requests in the meantime. Returns whether to
/// keep running.
#[cfg(feature = "local-model")]
fn wait_for_reload(jobs: &Receiver<Job>) -> bool {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Load => return true,
            Job::Stop => return false,
            Job::Complete(_) => {}
        }
    }
    false
}

/// The stub worker for builds without the `local-model` feature: it answers
/// every request with the reason there is no model, so the UI has something
/// truthful to display rather than silently never suggesting anything.
#[cfg(not(feature = "local-model"))]
fn run_worker(
    _config: AiConfig,
    jobs: Receiver<Job>,
    replies: Sender<Reply>,
    status: Arc<Mutex<Status>>,
    _generation: Arc<AtomicU64>,
) {
    const REASON: &str = "built without the `local-model` feature";
    while let Ok(job) = jobs.recv() {
        // Reported on first use rather than at startup, so a completer nobody
        // asked anything of still reads as `Cold` — the same as a real one.
        set_status(&status, Status::Failed(REASON.to_string()));
        match latest(job, &jobs) {
            Job::Stop => return,
            Job::Load => {}
            Job::Complete(req) => {
                let _ = replies.send(Reply { seq: req.seq, result: Err(REASON.to_string()) });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(seq: u64) -> Request {
        Request { seq, prefix: "fn main() {\n".into(), suffix: "\n}".into(), filename: None }
    }

    #[test]
    fn a_backlog_collapses_to_the_newest_request() {
        // The worker generates for seconds while the user types; working
        // through every intermediate context would make it permanently behind.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Job::Complete(req(2))).unwrap();
        tx.send(Job::Complete(req(3))).unwrap();
        let Job::Complete(got) = latest(Job::Complete(req(1)), &rx) else {
            panic!("expected a completion")
        };
        assert_eq!(got.seq, 3);
    }

    #[test]
    fn a_stop_beats_anything_queued_behind_it() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Job::Stop).unwrap();
        tx.send(Job::Complete(req(9))).unwrap();
        assert!(matches!(latest(Job::Complete(req(1)), &rx), Job::Stop));
    }

    #[test]
    fn a_queued_preload_does_not_displace_a_completion() {
        // `:AILoad` while typing must not throw away the pending suggestion —
        // the completion loads the model on its own anyway.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Job::Load).unwrap();
        let Job::Complete(got) = latest(Job::Complete(req(4)), &rx) else {
            panic!("expected the completion to survive")
        };
        assert_eq!(got.seq, 4);
    }

    #[test]
    fn context_is_capped_from_the_ends_furthest_from_the_cursor() {
        let cfg = AiConfig { max_prefix_chars: 5, max_suffix_chars: 3, ..Default::default() };
        let request = Request {
            seq: 1,
            prefix: "0123456789".into(),
            suffix: "abcdef".into(),
            filename: Some("src/lib.rs".into()),
        };
        let (prefix, suffix) = prompt_parts(&request, &cfg);
        assert_eq!(prefix, "56789", "kept the text nearest the cursor");
        assert_eq!(suffix, "abc");
    }

    #[test]
    fn the_prompt_is_exactly_the_code_around_the_cursor() {
        // Nothing is spliced in around it — see `prompt_parts`.
        let (prefix, suffix) = prompt_parts(&req(1), &AiConfig::default());
        assert_eq!(prefix, "fn main() {\n");
        assert_eq!(suffix, "\n}");
    }

    #[test]
    fn status_describes_itself_for_the_status_line() {
        assert_eq!(Status::Cold.badge(), None);
        assert_eq!(Status::Busy.badge(), Some("AI …"));
        assert!(Status::Failed("gated".into()).is_failed());
        assert!(Status::Loading("downloading".into()).describe().contains("downloading"));
    }

    #[test]
    fn polling_an_idle_completer_yields_nothing_and_does_not_block() {
        // Constructing one must not touch the network or the weights.
        let c = Completer::new(AiConfig::default());
        assert!(c.poll().is_none());
        assert_eq!(c.status(), Status::Cold);
    }
}
