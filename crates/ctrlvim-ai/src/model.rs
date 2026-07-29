//! The CodeGemma backend: candle for the tensors, `hf-hub` for the weights.
//!
//! CodeGemma-2B is architecturally Gemma-1 with a code-heavy training mix and
//! four extra fill-in-the-middle control tokens, so it loads through
//! `candle_transformers::models::gemma` unchanged — the only CodeGemma-specific
//! parts are the prompt format (see [`crate::prompt`]) and the stop tokens.
//!
//! Everything here is synchronous and single-threaded on purpose: it runs on
//! the worker thread [`crate::Completer`] owns, and the editor never touches
//! it. The one concession to the outside world is the `keep_going` callback
//! threaded through generation, which lets a superseded request abandon its
//! remaining tokens instead of burning CPU on a completion nobody will see.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use candle_core::quantized::gguf_file;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::models::gemma::{Config, Model};
use candle_transformers::utils::apply_repeat_penalty;
use tokenizers::Tokenizer;

use crate::config::{AiConfig, DevicePref, GgufSource, ModelSource, Precision};

/// Tokens that end a completion, by name. Ids are resolved against the
/// tokenizer at load time, since they differ between Gemma releases.
const STOP_TOKENS: &[&str] = &["<eos>", "<|file_separator|>", "<end_of_turn>", "<|fim_prefix|>"];

/// The loaded weights, in whichever precision they came in.
///
/// Both arms answer the same question — logits for the last position, given
/// input ids and a cache offset — so generation doesn't branch on this beyond
/// the two-line dispatch below.
enum Weights {
    /// Full-precision safetensors via `candle_transformers::models::gemma`.
    Full(Model),
    /// A 4/5/8-bit GGUF via [`crate::quantized_gemma`].
    Quantized(Box<crate::quantized_gemma::QuantizedGemma>),
}

impl Weights {
    fn forward(&mut self, input_ids: &Tensor, index_pos: usize) -> candle_core::Result<Tensor> {
        match self {
            Weights::Full(m) => m.forward(input_ids, index_pos),
            Weights::Quantized(m) => m.forward(input_ids, index_pos),
        }
    }

    fn clear_kv_cache(&mut self) {
        match self {
            Weights::Full(m) => m.clear_kv_cache(),
            Weights::Quantized(m) => m.clear_kv_cache(),
        }
    }
}

/// A loaded model, ready to complete.
pub struct CodeGemma {
    model: Weights,
    tokenizer: Tokenizer,
    device: Device,
    stop_ids: Vec<u32>,
    /// A short human-readable note about what was actually loaded, for
    /// `:AIStatus`.
    pub description: String,
}

impl CodeGemma {
    /// Resolve the weights (downloading them if they aren't cached), then build
    /// the model. This is the slow call: several gigabytes on a cold cache.
    ///
    /// `progress` is called with coarse stage descriptions so the editor can
    /// show something other than a frozen status line.
    pub fn load(
        source: &ModelSource,
        device: DevicePref,
        mut progress: impl FnMut(&str),
    ) -> Result<Self, String> {
        let device = pick_device(device)?;

        // The tokenizer always comes from the safetensors side, even for a GGUF
        // load: a GGUF embeds its vocabulary as raw metadata arrays, not as
        // something `tokenizers` can construct a BPE model from. It is ~17MB
        // against gigabytes of weights, so this costs nothing worth saving.
        progress("resolving tokenizer");
        let tokenizer_path = resolve_tokenizer(source)?;
        progress("loading tokenizer");
        let tokenizer =
            Tokenizer::from_file(&tokenizer_path).map_err(|e| format!("tokenizer: {e}"))?;

        let (model, origin) = match &source.gguf {
            Some(gguf) => (Self::load_gguf(gguf, &device, &mut progress)?, gguf.describe()),
            None => Self::load_safetensors(source, &device, &mut progress)?,
        };

        let stop_ids = STOP_TOKENS
            .iter()
            .filter_map(|t| tokenizer.token_to_id(t))
            .collect::<Vec<_>>();

        // CodeGemma adds four fill-in-the-middle tokens to the stock Gemma
        // vocabulary. Without them the prompt still *runs* — the markers just
        // get byte-pair-encoded as ordinary text — and the completions are
        // quietly much worse, which is a miserable thing to debug. Say so.
        let fim_native = tokenizer.token_to_id(crate::prompt::FIM_MIDDLE).is_some();
        let precision = match (&source.gguf, &model) {
            (Some(_), _) => "quantized".to_string(),
            (None, _) => source.precision.as_str().to_string(),
        };
        let description = format!(
            "{} ({}, {}){}",
            origin,
            precision,
            describe_device(&device),
            if fim_native { "" } else { " — no FIM tokens; is this a CodeGemma checkpoint?" }
        );
        Ok(CodeGemma { model, tokenizer, device, stop_ids, description })
    }

    /// Load a GGUF checkpoint, fetching it from the hub if it isn't local.
    fn load_gguf(
        gguf: &GgufSource,
        device: &Device,
        progress: &mut impl FnMut(&str),
    ) -> Result<Weights, String> {
        let path = match gguf {
            GgufSource::Path(p) => {
                if !p.exists() {
                    return Err(format!("{} not found", p.display()));
                }
                p.clone()
            }
            GgufSource::Hub { repo, file } => {
                progress("downloading quantized weights");
                hub_file(repo, "main", file)?
            }
        };
        progress("loading quantized weights");
        let mut reader = std::fs::File::open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let content = gguf_file::Content::read(&mut reader)
            .map_err(|e| format!("{}: not a readable GGUF: {e}", path.display()))?;
        let model = crate::quantized_gemma::QuantizedGemma::from_gguf(content, &mut reader, device)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Weights::Quantized(Box::new(model)))
    }

    /// Load full-precision safetensors, returning the weights and where they
    /// came from.
    fn load_safetensors(
        source: &ModelSource,
        device: &Device,
        progress: &mut impl FnMut(&str),
    ) -> Result<(Weights, String), String> {
        progress("resolving model files");
        let files = resolve(source)?;

        progress("reading config");
        let config_text = std::fs::read_to_string(&files.config)
            .map_err(|e| format!("{}: {e}", files.config.display()))?;
        let config: Config =
            serde_json::from_str(&config_text).map_err(|e| format!("config.json: {e}"))?;

        progress("loading weights");
        let dtype = match source.precision {
            Precision::Bf16 => DType::BF16,
            Precision::F16 => DType::F16,
            Precision::F32 => DType::F32,
        };
        // Safety: mmap-ing the weight files assumes nothing else truncates them
        // while the editor runs, which is the same assumption every candle
        // example makes.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&files.weights, dtype, device)
                .map_err(|e| format!("weights: {e}"))?
        };
        let model = Model::new(false, &config, vb).map_err(|e| format!("model: {e}"))?;
        Ok((Weights::Full(model), files.origin))
    }

    /// Complete the text between `prefix` and `suffix`.
    ///
    /// Stops at the first stop token, the token budget, the time budget, or
    /// when `keep_going` returns false — whichever comes first. A cancelled or
    /// timed-out generation still returns whatever it produced, because a
    /// partial line of ghost text is more useful than nothing.
    pub fn complete(
        &mut self,
        prefix: &str,
        suffix: &str,
        cfg: &AiConfig,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<String, String> {
        let prompt = crate::prompt::fim(prefix, suffix);
        let encoded = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| format!("tokenize: {e}"))?;
        let mut tokens = encoded.get_ids().to_vec();
        if tokens.is_empty() {
            return Ok(String::new());
        }

        // A fresh completion is a fresh context: the cache still holds the
        // previous request's keys, which belong to a different prompt.
        self.model.clear_kv_cache();

        let mut logits_processor = LogitsProcessor::from_sampling(
            cfg.seed,
            if cfg.temperature <= 0.0 {
                Sampling::ArgMax
            } else {
                match cfg.top_p {
                    Some(p) => Sampling::TopP { p, temperature: cfg.temperature },
                    None => Sampling::All { temperature: cfg.temperature },
                }
            },
        );

        let deadline = Instant::now() + Duration::from_millis(cfg.max_millis);
        let prompt_len = tokens.len();
        let mut generated: Vec<u32> = Vec::with_capacity(cfg.max_tokens);

        for step in 0..cfg.max_tokens {
            if !keep_going() || Instant::now() >= deadline {
                break;
            }
            // The first pass feeds the whole prompt; every later one feeds just
            // the token sampled last, with the KV cache carrying the rest.
            let (input, offset) = if step == 0 {
                (&tokens[..], 0)
            } else {
                (&tokens[tokens.len() - 1..], tokens.len() - 1)
            };
            let input = Tensor::new(input, &self.device)
                .map_err(err)?
                .unsqueeze(0)
                .map_err(err)?;
            let logits = self.model.forward(&input, offset).map_err(err)?;
            let logits = logits.squeeze(0).map_err(err)?.squeeze(0).map_err(err)?;
            let logits = logits.to_dtype(DType::F32).map_err(err)?;
            let logits = if cfg.repeat_penalty == 1.0 || generated.is_empty() {
                logits
            } else {
                // Penalize over what *this call* has generated, never over the
                // prompt.
                //
                // Penalizing the prompt is the natural-sounding choice and it
                // is badly wrong for code. Completing
                // `def add(a, b): "sum of a and b"; return ` would penalize the
                // tokens `a` and `b` precisely because the surrounding code
                // establishes them — and the model, steered away from the only
                // correct answer, returns something like
                // `int((float)(b) + (1.0 * float))`. Repeating identifiers from
                // context is what code *is*. The degenerate loops this is here
                // to break are all self-repetition within the completion, which
                // this still catches.
                let from = generated.len().saturating_sub(cfg.repeat_last_n);
                apply_repeat_penalty(&logits, cfg.repeat_penalty, &generated[from..]).map_err(err)?
            };

            let next = logits_processor.sample(&logits).map_err(err)?;
            if self.stop_ids.contains(&next) {
                break;
            }
            tokens.push(next);
            generated.push(next);
        }

        if generated.is_empty() {
            return Ok(String::new());
        }
        debug_assert_eq!(tokens.len(), prompt_len + generated.len());
        // Decoding the generated ids as a group (rather than one at a time)
        // keeps multi-token characters and the leading-space convention intact.
        // Special tokens are skipped so a stray `<pad>` can't be drawn as ghost
        // text; the textual stop markers `clean` looks for still come through,
        // because on a non-CodeGemma tokenizer they aren't special at all.
        self.tokenizer
            .decode(&generated, true)
            .map_err(|e| format!("decode: {e}"))
    }
}

fn err(e: candle_core::Error) -> String {
    e.to_string()
}

/// The files a load needs, plus where they came from.
#[derive(Debug)]
struct Files {
    config: PathBuf,
    weights: Vec<PathBuf>,
    origin: String,
}

/// Find `config.json`, `tokenizer.json`, and the safetensors shards — from a
/// local directory if one was configured, otherwise from the Hugging Face hub
/// (which caches, so only the first run pays for the download).
fn resolve(source: &ModelSource) -> Result<Files, String> {
    if let Some(dir) = &source.path {
        return resolve_local(dir);
    }
    resolve_hub(source)
}

fn resolve_local(dir: &Path) -> Result<Files, String> {
    let config = dir.join("config.json");
    if !config.exists() {
        return Err(format!("{} not found", config.display()));
    }
    let mut weights: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "safetensors"))
        .collect();
    // Shards are named `model-00001-of-00002.safetensors`; loading them in a
    // directory-listing order would still work (the var builder indexes by
    // name), but a stable order makes failures reproducible.
    weights.sort();
    if weights.is_empty() {
        return Err(format!("no .safetensors files in {}", dir.display()));
    }
    Ok(Files { config, weights, origin: dir.display().to_string() })
}

/// Fetch one file from a hub repo, using the local cache when it's there.
///
/// `from_env` picks up `HF_TOKEN` and the CLI's cached token, which is what a
/// gated repo like `google/codegemma-2b` needs.
fn hub_file(repo_id: &str, revision: &str, name: &str) -> Result<PathBuf, String> {
    use hf_hub::api::sync::ApiBuilder;
    use hf_hub::{Repo, RepoType};

    let api = ApiBuilder::from_env()
        .with_progress(false)
        .build()
        .map_err(|e| format!("hugging face api: {e}"))?;
    api.repo(Repo::with_revision(
        repo_id.to_string(),
        RepoType::Model,
        revision.to_string(),
    ))
    .get(name)
    .map_err(|e| explain_hub_error(repo_id, name, &e.to_string()))
}

/// Where `tokenizer.json` comes from: the local model directory if one is
/// configured, otherwise the hub repo.
fn resolve_tokenizer(source: &ModelSource) -> Result<PathBuf, String> {
    if let Some(dir) = &source.path {
        let path = dir.join("tokenizer.json");
        if !path.exists() {
            return Err(format!("{} not found", path.display()));
        }
        return Ok(path);
    }
    hub_file(&source.repo, &source.revision, "tokenizer.json")
}

fn resolve_hub(source: &ModelSource) -> Result<Files, String> {
    use hf_hub::api::sync::ApiBuilder;
    use hf_hub::{Repo, RepoType};

    let api = ApiBuilder::from_env()
        .with_progress(false)
        .build()
        .map_err(|e| format!("hugging face api: {e}"))?;
    let repo = api.repo(Repo::with_revision(
        source.repo.clone(),
        RepoType::Model,
        source.revision.clone(),
    ));
    let get = |name: &str| -> Result<PathBuf, String> {
        repo.get(name).map_err(|e| explain_hub_error(&source.repo, name, &e.to_string()))
    };

    let config = get("config.json")?;
    // Multi-shard repos ship an index naming the pieces; single-file ones
    // don't, so a missing index means "there is exactly one shard".
    let weights = match repo.get("model.safetensors.index.json") {
        Ok(index) => {
            let text = std::fs::read_to_string(&index).map_err(|e| e.to_string())?;
            let mut names = shard_names(&text)?;
            names.sort();
            names.iter().map(|n| get(n)).collect::<Result<Vec<_>, _>>()?
        }
        Err(_) => vec![get("model.safetensors")?],
    };
    Ok(Files { config, weights, origin: format!("{}@{}", source.repo, source.revision) })
}

/// The distinct shard filenames named by a `model.safetensors.index.json`.
fn shard_names(index_json: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(index_json).map_err(|e| format!("weight index: {e}"))?;
    let map = value
        .get("weight_map")
        .and_then(|m| m.as_object())
        .ok_or_else(|| "weight index has no weight_map".to_string())?;
    let mut names: Vec<String> = map
        .values()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return Err("weight index names no files".to_string());
    }
    Ok(names)
}

/// Turn a hub failure into something the user can act on. The overwhelmingly
/// common one is the license gate on `google/codegemma-2b`, which surfaces as a
/// bare 401/403 and is otherwise baffling.
fn explain_hub_error(repo: &str, file: &str, error: &str) -> String {
    let denied = error.contains("401")
        || error.contains("403")
        || error.to_lowercase().contains("unauthorized")
        || error.to_lowercase().contains("forbidden")
        || error.to_lowercase().contains("gated");
    if denied {
        return crate::config::gated_repo_help(repo);
    }
    format!("{repo}/{file}: {error}")
}

fn pick_device(pref: DevicePref) -> Result<Device, String> {
    match pref {
        DevicePref::Cpu => Ok(Device::Cpu),
        DevicePref::Cuda(n) => Device::new_cuda(n)
            .map_err(|e| format!("cuda:{n} unavailable: {e} (build ctrlvim with --features ai-cuda)")),
        DevicePref::Metal => Device::new_metal(0)
            .map_err(|e| format!("metal unavailable: {e} (build ctrlvim with --features ai-metal)")),
        // Try the accelerators this build actually has compiled in, quietly
        // falling back rather than failing: "it ran slowly" beats "it refused".
        DevicePref::Auto => Ok(Device::new_cuda(0)
            .or_else(|_| Device::new_metal(0))
            .unwrap_or(Device::Cpu)),
    }
}

fn describe_device(d: &Device) -> &'static str {
    match d {
        Device::Cpu => "cpu",
        Device::Cuda(_) => "cuda",
        Device::Metal(_) => "metal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_names_are_deduplicated_from_the_weight_map() {
        // Every tensor maps to a file; hundreds of entries name a handful of
        // shards, and downloading one per tensor would be absurd.
        let index = r#"{
            "metadata": {"total_size": 1},
            "weight_map": {
                "model.layers.0.q": "model-00001-of-00002.safetensors",
                "model.layers.1.q": "model-00001-of-00002.safetensors",
                "model.layers.9.q": "model-00002-of-00002.safetensors"
            }
        }"#;
        assert_eq!(
            shard_names(index).unwrap(),
            vec![
                "model-00001-of-00002.safetensors".to_string(),
                "model-00002-of-00002.safetensors".to_string()
            ]
        );
    }

    #[test]
    fn a_malformed_weight_index_is_an_error_not_a_panic() {
        assert!(shard_names("not json").is_err());
        assert!(shard_names(r#"{"weight_map": {}}"#).is_err());
    }

    #[test]
    fn a_gated_repo_is_explained_rather_than_reported_as_a_status_code() {
        let msg = explain_hub_error("google/codegemma-2b", "config.json", "request error: 403");
        assert!(msg.contains("gated"), "got {msg}");
        assert!(msg.contains("HF_TOKEN"), "got {msg}");
        let other = explain_hub_error("x/y", "config.json", "connection refused");
        assert!(other.contains("connection refused"));
    }

    #[test]
    fn a_missing_local_directory_names_the_file_it_wanted() {
        let err = resolve_local(Path::new("/nonexistent/ctrlvim-model")).unwrap_err();
        assert!(err.contains("config.json"), "got {err}");
    }

    #[test]
    fn codegemmas_real_config_deserializes_into_the_gemma_model() {
        // The verbatim `config.json` CodeGemma-2B ships. This is the assumption
        // the whole backend rests on — that CodeGemma is Gemma-1 as far as
        // candle is concerned — and it can be checked without the 5GB of
        // weights that would otherwise be the first thing to find out.
        let json = r#"{
          "architectures": ["GemmaForCausalLM"],
          "attention_bias": false,
          "attention_dropout": 0.0,
          "bos_token_id": 2,
          "eos_token_id": 1,
          "head_dim": 256,
          "hidden_act": "gelu",
          "hidden_activation": null,
          "hidden_size": 2048,
          "initializer_range": 0.02,
          "intermediate_size": 16384,
          "max_position_embeddings": 8192,
          "model_type": "gemma",
          "num_attention_heads": 8,
          "num_hidden_layers": 18,
          "num_key_value_heads": 1,
          "pad_token_id": 0,
          "rms_norm_eps": 1e-06,
          "rope_theta": 10000.0,
          "torch_dtype": "bfloat16",
          "vocab_size": 256000
        }"#;
        let cfg: Config = serde_json::from_str(json).expect("parses as a gemma config");
        assert_eq!(cfg.num_hidden_layers, 18);
        assert_eq!(cfg.head_dim, 256);
        // Multi-query attention: one KV head shared by all eight query heads.
        assert_eq!(cfg.num_key_value_heads, 1);
        // CodeGemma sets `hidden_act` and leaves `hidden_activation` null;
        // candle rejects both-or-neither, so this pairing matters.
        assert!(cfg.hidden_act.is_some() && cfg.hidden_activation.is_none());
    }

    #[test]
    fn cpu_is_always_available() {
        assert!(matches!(pick_device(DevicePref::Cpu), Ok(Device::Cpu)));
        assert!(pick_device(DevicePref::Auto).is_ok(), "auto never fails");
    }
}
