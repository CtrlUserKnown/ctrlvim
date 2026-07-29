# Inline AI suggestions

ctrlvim can propose code as you type, Copilot-style: grey "ghost text" ahead of
the cursor that `<Tab>` accepts and the next keystroke replaces. The model runs
**locally and in-process** — CodeGemma-2B on [candle](https://github.com/huggingface/candle),
no API key, no network at edit time, nothing leaving the machine.

## Turning it on and off

It is off at **two** levels, and both have to be switched for a suggestion to
appear.

### 1. Build it in

The backend is not compiled unless you ask for it:

```sh
cargo build -p ctrlvim --release --features ai
```

candle and its dependency tree are minutes of compile time (and do not build at
all on Apple silicon with a rustc older than the one that stabilized
`stdarch_neon_f16`), which is too much to charge every build for a feature that
is also off at runtime. A binary built without it still shows the settings row
and still answers `:AI`; suggestions just report "built without the
`local-model` feature" rather than silently never appearing. See
[Where the pieces live](#where-the-pieces-live).

If you want GPU support, `ai-cuda` / `ai-metal` below imply `ai` — you do not
need both flags.

### 2. Switch it on

Even in a build that has the backend, it is **off by default**, because the
first suggestion downloads **~5GB** of weights (see
[Disk and memory](#disk-and-memory) — this is *not* the 1.6GB you may be
expecting). Three ways to switch it:

- **Settings tab** — Dashboard → `settings` (`2`), then the **Inline AI
  suggestions** row: `Enter`/`Space`/click, or `a` from anywhere on the tab.
  This is the persistent one: it writes `[ai] enabled` back to
  `~/.config/ctrlvim/config.toml`, so the choice survives a restart. The
  checkbox tracks whether suggestions are actually running, not just what the
  file says.
- **`:AI`** — toggles for this session only (`:AI on` / `:AI off` to be
  explicit), the way `:set mouse` is to the mouse checkbox.
- **The config file** — directly:

  ```toml
  [ai]
  enabled = true
  ```

Toggling it off drops the worker thread and the loaded weights; toggling it
back on reloads them from the local cache, without re-downloading.

## Keys

Ghost text only claims keys while it is actually on screen. With nothing
suggested, `<Tab>` inserts a tab and `<C-e>` does what it always did.

| Key | Effect |
| --- | --- |
| `<Tab>` | Accept the whole suggestion |
| `<C-l>` | Accept one word of it, keep the rest |
| `<C-j>` | Accept one line of it, keep the rest |
| `<C-e>` | Dismiss it (stays in Insert mode, and doesn't immediately re-ask) |
| `<Esc>` | Leave Insert mode; the suggestion goes with it |

## Commands

| Command | Effect |
| --- | --- |
| `:AI` | Toggle suggestions (`:AI on` / `:AI off` to be explicit) |
| `:AISuggest` | Ask for a completion at the cursor now, skipping the idle delay |
| `:AIStatus` | Report where the model is: not loaded, downloading, ready, or why it failed |
| `:AILoad` | Start loading the weights now, rather than on the first suggestion |

The status line carries a marker while suggestions are armed: `AI` (ready),
`AI ↓` (loading), `AI …` (thinking), `AI !` (failed — `:AIStatus` says why).

## The model

The default is [`google/codegemma-2b`](https://huggingface.co/google/codegemma-2b):
the *base* 2B checkpoint, which is the one trained for fill-in-the-middle. That
is what inline completion is — the model sees the code on both sides of the
cursor, not just what comes before it:

```
<|fim_prefix|>{code before the cursor}<|fim_suffix|>{code after}<|fim_middle|>
```

### The license gate

`google/codegemma-2b` is a **gated** repository. The shipped defaults avoid it
entirely — weights come from an ungated GGUF repo and `tokenizer.json` from an
ungated mirror — so this only bites if you point `repo` back at Google's. To do
that you need to:

1. Accept the license at <https://huggingface.co/google/codegemma-2b>.
2. Make the token available, either by `huggingface-cli login` or by exporting
   `HF_TOKEN`.

Without this the load fails with a 401/403; `:AIStatus` will say so in as many
words rather than reporting a bare status code.

If you'd rather not do the license dance, point `repo` at an ungated re-upload
of the same weights — [`unsloth/codegemma-2b`](https://huggingface.co/unsloth/codegemma-2b)
has the layout this expects and downloads without a token:

```toml
[ai.model]
repo = "unsloth/codegemma-2b"
```

That is a third-party mirror, so it is not the default; the official gated repo
is. Any repo works as long as it is the Gemma architecture and ships
`config.json`, `tokenizer.json`, and `.safetensors` weights.

Or skip the network entirely with weights already on disk:

```toml
[ai.model]
path = "~/models/codegemma-2b"
```

A local `path` needs `config.json`, `tokenizer.json`, and the `.safetensors`
shards in one directory — i.e. exactly what `huggingface-cli download` leaves
behind.

### Disk and memory

ctrlvim loads a **4-bit quantized GGUF by default** — the same 1.6GB file
Ollama's `codegemma:2b` tag ships:

| File | Size | Used by default |
| --- | --- | --- |
| `codegemma-2b-Q4_K_M.gguf` | 1.63 GB | **yes** |
| `model.safetensors`, bf16 | 5.01 GB | only with `gguf = ""` |

Resident memory is ~2.8GB: the 1.6GB of weights plus a 1.05GB f16 embedding
table (Gemma's vocabulary is 256,000 tokens, so the table is large no matter
what the weights are quantized to).

To load full precision instead:

```toml
[ai.model]
gguf = ""
```

Other quantizations work — anything in a Gemma-1 GGUF repo:

```toml
[ai.model]
gguf = "bartowski/codegemma-2b-GGUF:codegemma-2b-Q5_K_M.gguf"   # 1.84GB, a bit better
gguf = "bartowski/codegemma-2b-GGUF:codegemma-2b-Q3_K_M.gguf"   # 1.38GB, a bit worse
gguf = "~/models/codegemma-2b-Q4_K_M.gguf"                      # a local file
```

> Gemma-2, Gemma-3 and Llama GGUFs will **not** load — this is a Gemma-1
> decoder (`crates/ctrlvim-ai/src/quantized_gemma.rs`), written because
> candle-transformers ships `quantized_gemma3` and nothing for Gemma-1.

## Speed

Be realistic about this, especially on a CPU.

Measured on a 4-core laptop, quantized, ~20 lines of context:

| | Cost |
| --- | --- |
| Prompt prefill (~200 tokens) | ~35s |
| Each generated token | ~0.45s |
| Model load (cached weights) | a few seconds |

**Prefill dominates**, and it scales with how much context you send — which is
why `context_before` defaults to 20 lines rather than something generous.
Doubling the context roughly doubles the time to the first suggestion.

If that is too slow, in order of effect:

1. **Build with GPU support.** By far the biggest difference:
   ```sh
   cargo build -p ctrlvim --release --features ai-cuda    # NVIDIA
   cargo build -p ctrlvim --release --features ai-metal   # Apple silicon
   ```
2. **Build in release mode at all.** candle's quantized kernels are roughly an
   order of magnitude slower unoptimized; a debug build cannot finish a
   completion in reasonable time.
3. **Shrink the context** further (`context_before = 10`).
4. **Lower `max_tokens`** — you rarely accept more than a line or two anyway.

### Why it doesn't make the editor itself sluggish

Because inference is confined to `cores - 1` threads. candle parallelizes across
every core it can find, and a 2B model keeps them all busy for seconds at a
time — which made the *whole editor* feel slow, not just the suggestions.
Leaving one core for the UI thread costs a little completion speed and buys back
a responsive editor. Override with:

```toml
[ai]
threads = 8     # default: one fewer than the machine has
```

The other protections are unchanged: a completion is only requested once typing
pauses (`debounce_ms`), only one is ever in flight, a newer request abandons the
older one mid-token, and `max_millis` caps any single attempt.

## Full configuration

Every key is optional; the values below are the defaults.

```toml
[ai]
enabled          = false   # off until you ask
device           = "auto"  # auto | cpu | cuda | cuda:1 | metal
debounce_ms      = 350     # idle time before a completion is requested
max_tokens       = 64      # generation ceiling
max_millis       = 8000    # wall-clock ceiling for one completion
temperature      = 0.0     # 0 = greedy, which is what code completion wants
top_p            = 0.0     # nucleus sampling; 0 disables it
repeat_penalty   = 1.1     # 1.0 disables it
repeat_last_n    = 64      # window the penalty looks back over
seed             = 0
context_before   = 20      # lines of context before the cursor (prefill cost!)
context_after    = 6       # lines after
max_prefix_chars = 1200    # hard caps, so one huge line can't blow the budget
max_suffix_chars = 400
max_lines        = 8       # most lines of ghost text to display
# threads        = 3       # default: one fewer than the machine has

[ai.model]
# 4-bit quantized weights (1.6GB). Set to "" for full-precision safetensors.
gguf      = "bartowski/codegemma-2b-GGUF:codegemma-2b-Q4_K_M.gguf"
# Supplies tokenizer.json, and the weights when `gguf` is empty. Ungated.
repo      = "unsloth/codegemma-2b"
revision  = "main"
precision = "bf16"         # bf16 | f16 | f32; ignored when `gguf` is set
# path    = "~/models/codegemma-2b"   # skip the download entirely
```

`max_lines = 1` is worth knowing about: continuation rows of a multi-line
suggestion are drawn over the buffer rows below the cursor (one source line is
one screen row in this TUI, so there is nowhere to push the real text down to).
They are shaded to read as an overlay and vanish on accept or dismiss, but if
you'd rather never see it, keep suggestions strictly inline.

## How it fits together

The split is the same one the quickfix list and the tag table use — the engine
owns the data, the host owns the I/O:

- **`ctrlvim-editor` (`suggest.rs`)** owns what is being suggested, where it is
  anchored, when it goes stale, and what accepting part of it does to the
  buffer. It has never heard of candle. Every context change bumps a sequence
  number that a reply must still match to be shown, which is what stops a
  completion for three keystrokes ago appearing at the new cursor.
- **`ctrlvim-ai`** owns the model, the weights, and the thread they run on. It
  has never heard of buffers or cursors.
- **`ctrlvim-tui`** joins them: it debounces, submits, polls, and draws.

`ctrlvim-ai` builds without candle at all — which is the **default**, so an
ordinary `cargo build` never pays for it. `--features ai` on `ctrlvim` (or
`--features local-model` on `ctrlvim-ai` directly) pulls it in. Without it,
suggestions report "built without the `local-model` feature" rather than
silently never appearing.
