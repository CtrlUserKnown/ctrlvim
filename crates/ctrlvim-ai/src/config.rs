//! What to load and how hard to think.
//!
//! Defaults are chosen for the case that actually has to work out of the box: a
//! laptop CPU. CodeGemma-2B on CPU produces single-digit tokens per second, so
//! the knobs that matter most are the ones bounding *work* — context window,
//! token budget, and a wall-clock deadline — rather than sampling quality.

use std::path::PathBuf;

/// Weight precision. `f32` doubles memory and is only worth it on hardware with
/// no half-precision path at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// bfloat16 — the format CodeGemma was trained in. The default.
    Bf16,
    /// float16.
    F16,
    /// float32.
    F32,
}

impl Precision {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bf16" | "bfloat16" => Some(Precision::Bf16),
            "f16" | "fp16" | "float16" | "half" => Some(Precision::F16),
            "f32" | "fp32" | "float32" | "full" => Some(Precision::F32),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Precision::Bf16 => "bf16",
            Precision::F16 => "f16",
            Precision::F32 => "f32",
        }
    }
}

/// Which compute device to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicePref {
    /// Use an accelerator if this build has one compiled in, else the CPU.
    Auto,
    Cpu,
    /// CUDA device N. Requires the crate's `cuda` feature.
    Cuda(usize),
    /// Apple Metal. Requires the crate's `metal` feature.
    Metal,
}

impl DevicePref {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "auto" => Some(DevicePref::Auto),
            "cpu" => Some(DevicePref::Cpu),
            "metal" | "mps" => Some(DevicePref::Metal),
            "cuda" | "gpu" => Some(DevicePref::Cuda(0)),
            _ => s
                .strip_prefix("cuda:")
                .and_then(|n| n.parse().ok())
                .map(DevicePref::Cuda),
        }
    }
}

/// Where a quantized (GGUF) checkpoint comes from.
///
/// Spelled either as a local file, or as `repo:file` naming a Hugging Face
/// repository and the specific quantization within it — GGUF repos hold a
/// dozen quantizations of the same model, so the file has to be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GgufSource {
    /// A `.gguf` file already on disk.
    Path(PathBuf),
    /// `{repo}:{file}` on the Hugging Face hub.
    Hub { repo: String, file: String },
}

impl GgufSource {
    /// Parse the config spelling: `owner/repo:file.gguf`, or any path.
    ///
    /// The two forms have to be told apart from the string alone. A hub spec is
    /// `{owner}/{name}:{file}` — *exactly* one slash before the colon, and no
    /// leading `/`, `.`, or `~`, which are what start a path. Everything else is
    /// a path, which correctly catches both Unix paths (`/models/x.gguf`, no
    /// colon at all) and a Windows drive letter (`C:\models\x.gguf`, whose
    /// pre-colon `C` has no slash).
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        if let Some((repo, file)) = spec.rsplit_once(':') {
            let looks_like_repo = repo.matches('/').count() == 1
                && !repo.starts_with(['/', '.', '~'])
                && !repo.is_empty();
            if looks_like_repo && !file.is_empty() {
                return Some(GgufSource::Hub {
                    repo: repo.to_string(),
                    file: file.to_string(),
                });
            }
        }
        Some(GgufSource::Path(crate::config::expand_home(spec)))
    }

    /// How to describe this source in `:AIStatus`.
    pub fn describe(&self) -> String {
        match self {
            GgufSource::Path(p) => p.display().to_string(),
            GgufSource::Hub { repo, file } => format!("{repo}/{file}"),
        }
    }
}

/// Expand a leading `~/` against `$HOME`.
///
/// Duplicated from the frontend's own `expand_tilde` on purpose: a path can
/// reach this crate straight from an API caller that never went through
/// ctrlvim's config loader, and a literal `~` directory is never what anyone
/// meant.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

/// Where the weights come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSource {
    /// Hugging Face repo id, used when [`path`](Self::path) is unset.
    ///
    /// `google/codegemma-2b` is a **gated** repo: it downloads only for an
    /// account that has accepted Google's license, with the token in `HF_TOKEN`
    /// (or `~/.cache/huggingface/token`). Point `repo` at a mirror, or `path`
    /// at a local directory, to avoid that.
    pub repo: String,
    /// Git revision within the repo.
    pub revision: String,
    /// A local directory holding `config.json`, `tokenizer.json`, and the
    /// `.safetensors` shards. Wins over `repo` when set, and skips the network
    /// entirely.
    pub path: Option<PathBuf>,
    pub precision: Precision,
    /// A quantized checkpoint to load **instead of** the safetensors weights.
    ///
    /// When set, `repo`/`path` still supply `tokenizer.json` (GGUF embeds a
    /// vocabulary, but not in a form `tokenizers` can read) while the weights
    /// come from here. This is the default, because 4-bit is roughly a third of
    /// the memory and materially faster on a CPU — which is where this runs
    /// unless you built with `ai-cuda`/`ai-metal`.
    ///
    /// Set to `None` (`gguf = ""` in the config) to load the full-precision
    /// safetensors instead.
    pub gguf: Option<GgufSource>,
}

impl Default for ModelSource {
    fn default() -> Self {
        ModelSource {
            // The base (non-instruction-tuned) 2B model: the one trained for
            // fill-in-the-middle, which is what inline completion is.
            //
            // An ungated mirror rather than `google/codegemma-2b`, which is
            // gated behind a license acceptance and so fails on a fresh install
            // for everyone who hasn't done that. Only `tokenizer.json` (~17MB)
            // is fetched from here by default; the weights come from `gguf`.
            repo: "unsloth/codegemma-2b".to_string(),
            revision: "main".to_string(),
            path: None,
            precision: Precision::Bf16,
            gguf: Some(GgufSource::Hub {
                repo: "bartowski/codegemma-2b-GGUF".to_string(),
                // Q4_K_M is the usual default quantization: ~1.6GB, and the
                // point on the size/quality curve everyone else ships too
                // (it's what Ollama's `codegemma:2b` tag is).
                file: "codegemma-2b-Q4_K_M.gguf".to_string(),
            }),
        }
    }
}

/// Everything the completion worker needs.
#[derive(Debug, Clone, PartialEq)]
pub struct AiConfig {
    /// Offer inline suggestions at all.
    pub enabled: bool,
    pub model: ModelSource,
    pub device: DevicePref,
    /// Idle time after the last keystroke before a completion is requested.
    /// Too low and every character starts a generation the next one cancels.
    pub debounce_ms: u64,
    /// Hard ceiling on generated tokens.
    pub max_tokens: usize,
    /// Wall-clock budget for one completion. Generation stops at whatever it
    /// has when this runs out, which on CPU is the difference between "ghost
    /// text appeared" and "the editor seems stuck".
    pub max_millis: u64,
    /// Sampling temperature; 0 means greedy, which is what code completion
    /// generally wants.
    pub temperature: f64,
    /// Nucleus sampling cutoff, applied only when `temperature > 0`.
    pub top_p: Option<f64>,
    /// Penalty applied to recently generated tokens (1.0 disables it), with the
    /// window it looks back over. Stops the short loops a small model falls
    /// into when the context is repetitive.
    pub repeat_penalty: f32,
    pub repeat_last_n: usize,
    pub seed: u64,
    /// Lines of buffer context before and after the cursor.
    pub context_before: usize,
    pub context_after: usize,
    /// Character caps on the assembled context, so one pathological line can't
    /// blow the prefill budget on its own.
    pub max_prefix_chars: usize,
    pub max_suffix_chars: usize,
    /// Most lines of ghost text to display.
    pub max_lines: usize,
    /// How many CPU threads inference may use. `None` leaves one core free.
    ///
    /// This is a *responsiveness* setting, not a throughput one. candle
    /// parallelizes across every core it can find, and a 2B model does enough
    /// work to keep all of them busy for seconds at a time — which is why
    /// turning suggestions on made the whole editor feel sluggish, not just the
    /// suggestions. Leaving a core for the UI thread costs a little completion
    /// speed and buys back a responsive editor.
    ///
    /// Ignored when running on a GPU, where the CPU isn't the bottleneck.
    pub threads: Option<usize>,
}

/// The thread count to actually use: the configured value, or one less than the
/// machine has, floored at one.
pub fn resolve_threads(configured: Option<usize>, available: usize) -> usize {
    match configured {
        Some(n) => n.max(1),
        // On a single- or dual-core machine, giving up half the CPU would make
        // completions unusably slow; the floor keeps at least one worker.
        None => available.saturating_sub(1).max(1),
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            enabled: false,
            model: ModelSource::default(),
            device: DevicePref::Auto,
            debounce_ms: 350,
            max_tokens: 64,
            max_millis: 8_000,
            temperature: 0.0,
            top_p: None,
            repeat_penalty: 1.1,
            repeat_last_n: 64,
            seed: 0,
            // Deliberately small. Prefill — the single pass over the prompt
            // before the first token appears — dominates the cost of a
            // completion on a CPU, and it scales with prompt length: measured
            // on a 4-core laptop, ~400 prompt tokens cost ~58s while each
            // generated token cost ~0.45s. Context is therefore the most
            // expensive knob in this file, not the most generous one. Raise it
            // if you have a GPU (`ai-cuda` / `ai-metal`), where it is nearly
            // free.
            context_before: 20,
            context_after: 6,
            max_prefix_chars: 1_200,
            max_suffix_chars: 400,
            max_lines: 8,
            threads: None,
        }
    }
}

impl AiConfig {
    /// The display trimming implied by this config.
    pub fn trim(&self) -> crate::prompt::Trim {
        crate::prompt::Trim { max_lines: self.max_lines }
    }
}

/// What to tell someone whose model download was refused by the license gate.
///
/// This is the likeliest way the feature fails on a fresh install, and the
/// whole value of the message is the instructions — so it is deliberately
/// **several lines**. The first is a self-contained summary, because that is
/// all a status line has room for; a host with somewhere better to put the rest
/// (ctrlvim opens its output panel) shows the whole thing. Written as one long
/// line, it used to be clipped at the terminal edge, which left the user with
/// "is gated" and nowhere to go.
///
/// Lives here rather than in `model` so it is available even in builds without
/// the `local-model` feature — nothing about it needs candle.
pub fn gated_repo_help(repo: &str) -> String {
    format!(
        "{repo} is gated — Hugging Face refused the download.\n\
         \n\
         Two ways to fix it:\n\
         \n\
         1. Use it as published. Accept the license at\n   \
              https://huggingface.co/{repo}\n   \
            then authenticate, either by running\n   \
              huggingface-cli login\n   \
            or by setting HF_TOKEN in your environment.\n   \
            Then run  :AILoad  to try again.\n\
         \n\
         2. Use an ungated mirror of the same weights. Add this to\n   \
            ~/.config/ctrlvim/config.toml:\n\
         \n   \
              [ai.model]\n   \
              repo = \"unsloth/codegemma-2b\"\n\
         \n   \
            …or point `path` at a copy you already have on disk.\n\
         \n\
         See docs/ai.md for the full picture."
    )
}

/// Truncate `s` to at most `max` characters, keeping the **end** — the text
/// nearest the cursor is the text that matters for a prefix.
pub fn keep_tail(s: &str, max: usize) -> &str {
    let count = s.chars().count();
    if count <= max {
        return s;
    }
    let start = s
        .char_indices()
        .nth(count - max)
        .map(|(i, _)| i)
        .unwrap_or(0);
    &s[start..]
}

/// Truncate `s` to at most `max` characters, keeping the **start** — the
/// counterpart for a suffix.
pub fn keep_head(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_and_device_parse_the_spellings_people_actually_write() {
        assert_eq!(Precision::parse("BF16"), Some(Precision::Bf16));
        assert_eq!(Precision::parse("float32"), Some(Precision::F32));
        assert_eq!(Precision::parse("nonsense"), None);
        assert_eq!(DevicePref::parse("cpu"), Some(DevicePref::Cpu));
        assert_eq!(DevicePref::parse("cuda:1"), Some(DevicePref::Cuda(1)));
        assert_eq!(DevicePref::parse("gpu"), Some(DevicePref::Cuda(0)));
        assert_eq!(DevicePref::parse("wat"), None);
    }

    #[test]
    fn a_prefix_is_truncated_from_the_far_end_and_a_suffix_from_the_near_one() {
        assert_eq!(keep_tail("abcdef", 3), "def");
        assert_eq!(keep_head("abcdef", 3), "abc");
        assert_eq!(keep_tail("abc", 10), "abc");
        assert_eq!(keep_head("abc", 10), "abc");
    }

    #[test]
    fn truncation_lands_on_character_boundaries() {
        // Byte-slicing a multi-byte character would panic; these are the
        // strings that find it.
        assert_eq!(keep_tail("aé€b", 2), "€b");
        assert_eq!(keep_head("aé€b", 2), "aé");
    }

    #[test]
    fn thread_count_leaves_a_core_for_the_editor() {
        // The whole point: inference must not be able to claim every core.
        assert_eq!(resolve_threads(None, 8), 7);
        assert_eq!(resolve_threads(None, 4), 3);
        // …but never down to zero workers on a small machine.
        assert_eq!(resolve_threads(None, 1), 1);
        assert_eq!(resolve_threads(None, 2), 1);
        // An explicit setting wins, still floored at one.
        assert_eq!(resolve_threads(Some(16), 4), 16);
        assert_eq!(resolve_threads(Some(0), 4), 1);
    }

    #[test]
    fn a_gguf_spec_distinguishes_a_hub_file_from_a_local_path() {
        assert_eq!(
            GgufSource::parse("bartowski/codegemma-2b-GGUF:codegemma-2b-Q4_K_M.gguf"),
            Some(GgufSource::Hub {
                repo: "bartowski/codegemma-2b-GGUF".into(),
                file: "codegemma-2b-Q4_K_M.gguf".into(),
            })
        );
        assert_eq!(
            GgufSource::parse("/models/cg.gguf"),
            Some(GgufSource::Path("/models/cg.gguf".into()))
        );
        // A Windows drive letter: `C` before the colon has no slash.
        assert!(matches!(GgufSource::parse("C:/models/cg.gguf"), Some(GgufSource::Path(_))));
        // An absolute Unix path with a colon in the filename is still a path.
        assert!(matches!(GgufSource::parse("/models/a:b.gguf"), Some(GgufSource::Path(_))));
        // Two slashes is a path, not `owner/name`.
        assert!(matches!(GgufSource::parse("models/sub/x:y.gguf"), Some(GgufSource::Path(_))));
        // An explicit empty string is how a config says "no quantization".
        assert_eq!(GgufSource::parse(""), None);
        assert_eq!(GgufSource::parse("   "), None);
    }

    #[test]
    fn a_gguf_path_expands_a_leading_tilde() {
        let Some(GgufSource::Path(p)) = GgufSource::parse("~/models/cg.gguf") else {
            panic!("expected a path")
        };
        assert!(!p.starts_with("~"), "got {}", p.display());
        assert!(p.ends_with("models/cg.gguf"));
    }

    #[test]
    fn the_default_model_is_quantized_and_ungated() {
        // Both halves matter: quantized because a 5GB bf16 model on a CPU makes
        // the editor crawl, ungated because the official repo 401s on a fresh
        // install until the user accepts a license.
        let d = ModelSource::default();
        let Some(GgufSource::Hub { repo, file }) = &d.gguf else {
            panic!("the default should be quantized")
        };
        assert!(file.contains("Q4_K_M"), "got {file}");
        assert!(!repo.starts_with("google/"), "gated repos can't be the default");
        assert!(!d.repo.starts_with("google/"), "nor for the tokenizer: {}", d.repo);
    }

    #[test]
    fn suggestions_are_off_until_asked_for() {
        // A default build must not try to download 5GB of weights because the
        // user opened a file.
        assert!(!AiConfig::default().enabled);
    }
}
