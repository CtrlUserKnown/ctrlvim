//! End-to-end test of the real inference path: load weights off disk, tokenize
//! a fill-in-the-middle prompt, run the generation loop, and clean the output.
//!
//! Everything else in this crate's tests is pure logic, precisely so the suite
//! stays offline and fast. That leaves one thing unverified — whether the
//! candle plumbing actually *runs* — which is exactly the part that fails in
//! ways unit tests can't predict (a wrong tensor rank, a KV-cache offset off by
//! one, a tokenizer that doesn't know the FIM tokens).
//!
//! So this test exists but does not run by default. Point it at any local
//! Gemma-architecture checkpoint:
//!
//! ```sh
//! # A ~30MB random-weight Gemma is enough — this asserts the pipeline runs,
//! # not that the output is good:
//! hf download fxmarty/tiny-random-GemmaForCausalLM --local-dir /tmp/tiny-gemma
//! CTRLVIM_AI_TEST_MODEL=/tmp/tiny-gemma cargo test -p ctrlvim-ai -- --ignored
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ctrlvim_ai::{AiConfig, Completer, DevicePref, ModelSource, Precision, Request, Status};

/// The checkpoint to run against, or `None` to skip.
fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("CTRLVIM_AI_TEST_MODEL")?);
    assert!(dir.is_dir(), "CTRLVIM_AI_TEST_MODEL={} is not a directory", dir.display());
    Some(dir)
}

fn config(dir: PathBuf) -> AiConfig {
    AiConfig {
        enabled: true,
        model: ModelSource {
            path: Some(dir),
            // Test checkpoints are usually f32, and CPU f32 is the one
            // combination guaranteed to exist everywhere.
            precision: Precision::F32,
            ..ModelSource::default()
        },
        device: DevicePref::Cpu,
        max_tokens: 8,
        max_millis: 60_000,
        ..AiConfig::default()
    }
}

/// Block until the completer answers, or give up.
fn wait_for_reply(c: &Completer, timeout: Duration) -> ctrlvim_ai::Reply {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(reply) = c.poll() {
            return reply;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no reply within {timeout:?} (status: {})", c.status().describe());
}

#[test]
#[ignore = "needs CTRLVIM_AI_TEST_MODEL pointing at a local Gemma checkpoint"]
fn a_real_model_loads_and_completes() {
    let Some(dir) = model_dir() else { return };
    let completer = Completer::new(config(dir));
    assert_eq!(completer.status(), Status::Cold, "nothing loads until asked");

    completer.submit(Request {
        seq: 7,
        prefix: "fn add(a: i32, b: i32) -> i32 {\n    ".into(),
        suffix: "\n}\n".into(),
        filename: Some("add.rs".into()),
    });

    let reply = wait_for_reply(&completer, Duration::from_secs(180));
    assert_eq!(reply.seq, 7, "the staleness token comes back untouched");
    let text = reply.result.expect("generation should succeed");

    // The weights may be random, so *what* it said is not assertable — that it
    // ran, produced clean text, and left the model ready, is.
    assert!(!text.contains("<|fim_"), "control tokens must never reach the buffer: {text:?}");
    assert!(!text.contains('\r'));
    assert!(matches!(completer.status(), Status::Ready(_)), "{:?}", completer.status());
}

#[test]
#[ignore = "needs CTRLVIM_AI_TEST_MODEL pointing at a local Gemma checkpoint"]
fn a_second_completion_reuses_the_loaded_model() {
    // The KV cache is cleared per request; a second completion that produced
    // garbage (or panicked on a stale cache offset) would show up here.
    let Some(dir) = model_dir() else { return };
    let completer = Completer::new(config(dir));

    for seq in [1u64, 2] {
        completer.submit(Request {
            seq,
            prefix: format!("// request {seq}\nlet x = "),
            suffix: ";\n".into(),
            filename: None,
        });
        let reply = wait_for_reply(&completer, Duration::from_secs(180));
        assert_eq!(reply.seq, seq);
        assert!(reply.result.is_ok(), "{:?}", reply.result);
    }
}

#[test]
#[ignore = "needs CTRLVIM_AI_TEST_MODEL pointing at a local Gemma checkpoint"]
fn a_missing_checkpoint_fails_without_hanging() {
    // The failure path matters as much as the happy one: a bad `path` must
    // produce an error the user can read, not a worker stuck forever.
    let mut cfg = config(PathBuf::from("."));
    cfg.model.path = Some(PathBuf::from("/nonexistent/ctrlvim-model"));
    let completer = Completer::new(cfg);
    completer.submit(Request { seq: 1, prefix: "x".into(), suffix: "".into(), filename: None });

    let reply = wait_for_reply(&completer, Duration::from_secs(30));
    let err = reply.result.expect_err("a missing model is an error");
    assert!(err.contains("config.json"), "should name what it wanted: {err}");
    assert!(completer.status().is_failed());
}

// --- quantized (GGUF) ------------------------------------------------------

/// A `.gguf` to test against, or `None` to skip.
fn gguf_file() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("CTRLVIM_AI_TEST_GGUF")?);
    assert!(path.is_file(), "CTRLVIM_AI_TEST_GGUF={} is not a file", path.display());
    Some(path)
}

fn quantized_config(gguf: PathBuf) -> AiConfig {
    AiConfig {
        enabled: true,
        model: ModelSource {
            // The tokenizer still comes from the safetensors repo; the weights
            // come from the GGUF.
            gguf: Some(ctrlvim_ai::GgufSource::Path(gguf)),
            ..ModelSource::default()
        },
        device: DevicePref::Cpu,
        max_tokens: 24,
        max_millis: 120_000,
        ..AiConfig::default()
    }
}

#[test]
#[ignore = "needs CTRLVIM_AI_TEST_GGUF pointing at a CodeGemma .gguf"]
fn a_quantized_completion_is_coherent_code() {
    // This is the test that actually validates `quantized_gemma`. A transformer
    // wired up *almost* right — a missed `sqrt(hidden_size)` embedding scale, a
    // double-applied `1 + weight` on the norms, the wrong GeLU — still runs and
    // still returns tokens. It just returns noise. So this asserts on the
    // *content*: completing a function whose body is forced by its name.
    let Some(gguf) = gguf_file() else { return };
    let completer = Completer::new(quantized_config(gguf));

    completer.submit(Request {
        seq: 1,
        prefix: "def add(a, b):\n    \"\"\"Return the sum of a and b.\"\"\"\n    return ".into(),
        suffix: "\n".into(),
        filename: Some("math.py".into()),
    });
    let reply = wait_for_reply(&completer, Duration::from_secs(300));
    let text = reply.result.expect("generation should succeed");
    eprintln!("quantized completion: {text:?}");
    eprintln!("status: {}", completer.status().describe());

    assert!(!text.trim().is_empty(), "the model produced nothing");
    // `a + b` is the only sensible completion here. Accepting either operand
    // order keeps this about coherence rather than exact sampling.
    let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        squeezed.starts_with("a+b") || squeezed.starts_with("b+a"),
        "expected `a + b`, got {text:?} — the model is running but incoherent, \
         which points at the wiring in `quantized_gemma` rather than at sampling"
    );
    assert!(matches!(completer.status(), Status::Ready(_)));
}

#[test]
#[ignore = "needs CTRLVIM_AI_TEST_GGUF pointing at a CodeGemma .gguf"]
fn quantized_fill_in_the_middle_respects_the_suffix() {
    // FIM is the whole point: the model must use the code *after* the cursor.
    let Some(gguf) = gguf_file() else { return };
    let completer = Completer::new(quantized_config(gguf));
    completer.submit(Request {
        seq: 2,
        prefix: "def greet(name):\n    message = ".into(),
        suffix: "\n    return message\n".into(),
        filename: Some("greet.py".into()),
    });
    let reply = wait_for_reply(&completer, Duration::from_secs(300));
    let text = reply.result.expect("generation should succeed");
    eprintln!("fim completion: {text:?}");
    assert!(!text.trim().is_empty());
    // It must not re-emit the `return message` that already follows the cursor.
    assert!(!text.contains("return message"), "duplicated the suffix: {text:?}");
}
