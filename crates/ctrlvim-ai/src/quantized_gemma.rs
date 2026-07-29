//! A quantized Gemma-1 decoder, for loading CodeGemma from a GGUF file.
//!
//! candle-transformers ships `quantized_gemma3` but nothing for Gemma-1, and
//! CodeGemma *is* Gemma-1 — so the 1.6GB 4-bit checkpoints everyone has
//! (Ollama's `codegemma:2b`, the `*-GGUF` repos) had no loader. This is that
//! loader, modelled closely on candle's `quantized_gemma3` with the Gemma-3
//! specifics taken back out:
//!
//! | | Gemma-3 | Gemma-1 (here) |
//! |---|---|---|
//! | Q/K normalization | yes | no |
//! | Norm placement | pre *and* post | pre only |
//! | MLP activation | SwiGLU (SiLU) | GeGLU (tanh-approximate GeLU) |
//! | Attention | alternating local/global sliding window | plain causal |
//! | RoPE base | 1e6 (10e3 local) | 1e4 |
//!
//! # The `+1` on norm weights
//!
//! Gemma's RMSNorm scales by `1 + weight`, not `weight` — see
//! `candle_transformers::models::gemma`, which adds it explicitly. It is *not*
//! added here, because `llama.cpp`'s GGUF conversion bakes it into the stored
//! tensors. Adding it again would apply it twice and the model would emit
//! plausible-looking noise rather than failing loudly, so
//! `a_quantized_completion_is_coherent_code` in `tests/inference.rs` checks the
//! output is real code rather than merely that it ran.

use candle_core::quantized::{gguf_file, QMatMul, QTensor};
use candle_core::{DType, Device, IndexOp, Module, Result, Tensor, D};

/// Longest position the rotary tables are precomputed for. Gemma-1's trained
/// context is 8192; going past it would index past the table.
pub const MAX_SEQ_LEN: usize = 8192;

/// The token embedding table, held in f16.
///
/// This matters more for Gemma than for most models. The table is
/// `vocab × hidden` = 256,000 × 2048, and dequantizing it to f32 — which is
/// what candle's own quantized examples do — costs **2.1GB**, more than the
/// entire 1.6GB of quantized weights it accompanies. That single allocation ate
/// most of the memory saving quantization exists to provide.
///
/// f16 halves it, and costs nothing in quality: these are embedding lookups,
/// not accumulations. The result is cast up to f32 on the way out so the rest
/// of the model stays in one dtype.
#[derive(Debug, Clone)]
struct TokenEmbedding {
    weight: Tensor,
    hidden_size: usize,
}

impl TokenEmbedding {
    fn forward(&self, ids: &Tensor) -> Result<Tensor> {
        let (b_sz, seq_len) = ids.dims2()?;
        let flat = ids.flatten_all()?;
        self.weight
            .index_select(&flat, 0)?
            .reshape((b_sz, seq_len, self.hidden_size))?
            .to_dtype(DType::F32)
    }
}

/// Gemma's RMSNorm over a dequantized weight vector.
///
/// The weight is dequantized once at load time rather than per forward pass:
/// it is one vector per layer, so keeping it in f32 costs almost nothing and
/// saves a dequantize on every token.
#[derive(Debug, Clone)]
struct RmsNorm {
    weight: Tensor,
    eps: f64,
}

impl RmsNorm {
    fn from_qtensor(weight: QTensor, eps: f64) -> Result<Self> {
        let weight = weight.dequantize(&weight.device())?;
        Ok(RmsNorm { weight, eps })
    }
}

impl Module for RmsNorm {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        candle_nn::ops::rms_norm(&x.contiguous()?, &self.weight, self.eps as f32)
    }
}

/// Gemma's gated feed-forward block: `down(gelu(gate(x)) * up(x))`.
#[derive(Debug, Clone)]
struct Mlp {
    gate: QMatMul,
    up: QMatMul,
    down: QMatMul,
}

impl Module for Mlp {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let gate = self.gate.forward(xs)?;
        let up = self.up.forward(xs)?;
        // Gemma is GeGLU, and specifically the tanh approximation
        // (`gelu_pytorch_tanh`), which is what candle's `gelu` computes —
        // `gelu_erf` is the exact one and would be the wrong function here.
        let activated = gate.gelu()?;
        self.down.forward(&(activated * up)?)
    }
}

/// Precomputed rotary sin/cos tables, shared by every layer.
#[derive(Debug, Clone)]
struct RotaryEmbedding {
    sin: Tensor,
    cos: Tensor,
}

impl RotaryEmbedding {
    fn new(head_dim: usize, base: f32, device: &Device) -> Result<Self> {
        let theta: Vec<f32> = (0..head_dim)
            .step_by(2)
            .map(|i| 1f32 / base.powf(i as f32 / head_dim as f32))
            .collect();
        let theta = Tensor::new(theta.as_slice(), device)?;
        let idx_theta = Tensor::arange(0, MAX_SEQ_LEN as u32, device)?
            .to_dtype(DType::F32)?
            .reshape((MAX_SEQ_LEN, 1))?
            .matmul(&theta.reshape((1, theta.elem_count()))?)?;
        Ok(RotaryEmbedding { sin: idx_theta.sin()?, cos: idx_theta.cos()? })
    }

    fn apply(&self, q: &Tensor, k: &Tensor, index_pos: usize) -> Result<(Tensor, Tensor)> {
        let (_b, _h, seq_len, _d) = q.dims4()?;
        let cos = self.cos.narrow(0, index_pos, seq_len)?;
        let sin = self.sin.narrow(0, index_pos, seq_len)?;
        let q = candle_nn::rotary_emb::rope(&q.contiguous()?, &cos, &sin)?;
        let k = candle_nn::rotary_emb::rope(&k.contiguous()?, &cos, &sin)?;
        Ok((q, k))
    }
}

/// One transformer block's weights, plus its slice of the KV cache.
#[derive(Debug, Clone)]
struct Layer {
    attn_q: QMatMul,
    attn_k: QMatMul,
    attn_v: QMatMul,
    attn_output: QMatMul,
    attn_norm: RmsNorm,
    ffn_norm: RmsNorm,
    mlp: Mlp,
    n_head: usize,
    n_kv_head: usize,
    head_dim: usize,
    kv_cache: Option<(Tensor, Tensor)>,
}

impl Layer {
    fn forward_attn(
        &mut self,
        x: &Tensor,
        mask: Option<&Tensor>,
        index_pos: usize,
        rotary: &RotaryEmbedding,
    ) -> Result<Tensor> {
        let (b_sz, seq_len, _) = x.dims3()?;
        let q = self.attn_q.forward(x)?;
        let k = self.attn_k.forward(x)?;
        let v = self.attn_v.forward(x)?;

        // Gemma-2B's projections are *not* `hidden_size` wide: head_dim is 256
        // while hidden_size/n_head is 2048/8 = 256 for queries but the single
        // KV head makes k/v only 256 wide in total. Reshaping by head count and
        // head_dim (rather than by hidden_size) is what makes both work.
        let q = q.reshape((b_sz, seq_len, self.n_head, self.head_dim))?.transpose(1, 2)?;
        let k = k.reshape((b_sz, seq_len, self.n_kv_head, self.head_dim))?.transpose(1, 2)?;
        let v = v.reshape((b_sz, seq_len, self.n_kv_head, self.head_dim))?.transpose(1, 2)?;

        let (q, k) = rotary.apply(&q, &k, index_pos)?;

        let (k, v) = match &self.kv_cache {
            Some((kc, vc)) if index_pos > 0 => {
                (Tensor::cat(&[kc, &k], 2)?, Tensor::cat(&[vc, &v], 2)?)
            }
            _ => (k, v),
        };
        self.kv_cache = Some((k.clone(), v.clone()));

        // Multi-query attention: CodeGemma-2B has one KV head shared by all
        // eight query heads, so the cache is tiny but has to be broadcast out.
        let repeat = self.n_head / self.n_kv_head;
        let k = candle_transformers::utils::repeat_kv(k, repeat)?;
        let v = candle_transformers::utils::repeat_kv(v, repeat)?;

        let scale = 1f64 / (self.head_dim as f64).sqrt();
        let mut attn = (q.contiguous()?.matmul(&k.transpose(2, 3)?.contiguous()?)? * scale)?;
        if let Some(mask) = mask {
            attn = attn.broadcast_add(mask)?;
        }
        let attn = candle_nn::ops::softmax_last_dim(&attn)?;
        let out = attn.matmul(&v.contiguous()?)?;
        let out = out
            .transpose(1, 2)?
            .reshape((b_sz, seq_len, self.n_head * self.head_dim))?;
        self.attn_output.forward(&out)
    }
}

/// A quantized CodeGemma / Gemma-1 model.
#[derive(Debug, Clone)]
pub struct QuantizedGemma {
    tok_embeddings: TokenEmbedding,
    hidden_size: usize,
    layers: Vec<Layer>,
    norm: RmsNorm,
    output: QMatMul,
    rotary: RotaryEmbedding,
    device: Device,
}

impl QuantizedGemma {
    /// Read a GGUF file produced by `llama.cpp`'s Gemma conversion.
    pub fn from_gguf<R: std::io::Seek + std::io::Read>(
        ct: gguf_file::Content,
        reader: &mut R,
        device: &Device,
    ) -> Result<Self> {
        // Gemma-1 files are written with a `gemma.` prefix; accept `gemma2`
        // too, whose block structure this does not actually implement but
        // whose absence here would be a confusing "cannot find gemma.*" error.
        let prefix = ["gemma", "gemma2"]
            .iter()
            .find(|p| ct.metadata.contains_key(&format!("{p}.attention.head_count")))
            .copied()
            .ok_or_else(|| {
                candle_core::Error::Msg(
                    "not a Gemma GGUF (no `gemma.*` metadata) — CodeGemma is Gemma-1; \
                     a Gemma-2/3 or Llama file will not load here"
                        .to_string(),
                )
            })?;
        let md = |s: &str| -> Result<&gguf_file::Value> {
            let key = format!("{prefix}.{s}");
            ct.metadata
                .get(&key)
                .ok_or_else(|| candle_core::Error::Msg(format!("missing {key} in GGUF metadata")))
        };

        let n_head = md("attention.head_count")?.to_u32()? as usize;
        let n_kv_head = md("attention.head_count_kv")?.to_u32()? as usize;
        let block_count = md("block_count")?.to_u32()? as usize;
        let hidden_size = md("embedding_length")?.to_u32()? as usize;
        // Gemma's head_dim is an independent hyperparameter (256), *not*
        // hidden_size / n_head — assuming the latter silently mis-shapes every
        // projection.
        let head_dim = md("attention.key_length")?.to_u32()? as usize;
        let rms_eps = md("attention.layer_norm_rms_epsilon")?.to_f32()? as f64;
        let rope_base = md("rope.freq_base").and_then(|v| v.to_f32()).unwrap_or(10_000.);

        // f16, not f32 — see `TokenEmbedding`. `dequantize_f16` avoids ever
        // materializing the f32 copy, so peak memory never spikes either.
        let tok_embeddings = ct.tensor(reader, "token_embd.weight", device)?;
        let tok_embeddings = tok_embeddings
            .dequantize_f16(device)
            .or_else(|_| tok_embeddings.dequantize(device)?.to_dtype(DType::F16))?;
        let norm =
            RmsNorm::from_qtensor(ct.tensor(reader, "output_norm.weight", device)?, rms_eps)?;
        // Gemma ties the output projection to the embeddings, so a GGUF for it
        // usually has no `output.weight` at all.
        let output = match ct.tensor(reader, "output.weight", device) {
            Ok(t) => t,
            Err(_) => ct.tensor(reader, "token_embd.weight", device)?,
        };

        let rotary = RotaryEmbedding::new(head_dim, rope_base, device)?;
        let mut layers = Vec::with_capacity(block_count);
        for i in 0..block_count {
            let p = format!("blk.{i}");
            layers.push(Layer {
                attn_q: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.attn_q.weight"), device)?)?,
                attn_k: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.attn_k.weight"), device)?)?,
                attn_v: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.attn_v.weight"), device)?)?,
                attn_output: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.attn_output.weight"), device)?)?,
                attn_norm: RmsNorm::from_qtensor(ct.tensor(reader, &format!("{p}.attn_norm.weight"), device)?, rms_eps)?,
                ffn_norm: RmsNorm::from_qtensor(ct.tensor(reader, &format!("{p}.ffn_norm.weight"), device)?, rms_eps)?,
                mlp: Mlp {
                    gate: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.ffn_gate.weight"), device)?)?,
                    up: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.ffn_up.weight"), device)?)?,
                    down: QMatMul::from_qtensor(ct.tensor(reader, &format!("{p}.ffn_down.weight"), device)?)?,
                },
                n_head,
                n_kv_head,
                head_dim,
                kv_cache: None,
            });
        }

        Ok(QuantizedGemma {
            tok_embeddings: TokenEmbedding { weight: tok_embeddings, hidden_size },
            hidden_size,
            layers,
            norm,
            output: QMatMul::from_qtensor(output)?,
            rotary,
            device: device.clone(),
        })
    }

    /// Additive causal mask: `0` where attention is allowed, `-inf` where not.
    ///
    /// Added to the scores rather than used as a `where_cond`, which is why it
    /// is built in the model's compute dtype and cached per call — the shape
    /// depends on how much of the prompt is already in the KV cache.
    fn causal_mask(&self, b_sz: usize, seq_len: usize, index_pos: usize) -> Result<Tensor> {
        let mask: Vec<f32> = (0..seq_len)
            .flat_map(|i| {
                (0..seq_len).map(move |j| if i < j { f32::NEG_INFINITY } else { 0f32 })
            })
            .collect();
        let mask = Tensor::from_slice(&mask, (seq_len, seq_len), &self.device)?;
        // Everything already in the cache is visible to every new position.
        let mask = if index_pos > 0 {
            let prefix = Tensor::zeros((seq_len, index_pos), DType::F32, &self.device)?;
            Tensor::cat(&[&prefix, &mask], D::Minus1)?
        } else {
            mask
        };
        mask.reshape((1, 1, seq_len, seq_len + index_pos))?
            .broadcast_as((b_sz, 1, seq_len, seq_len + index_pos))
    }

    /// Logits for the **last** position only, matching the unquantized
    /// `gemma::Model::forward` this stands in for.
    pub fn forward(&mut self, input_ids: &Tensor, index_pos: usize) -> Result<Tensor> {
        let (b_sz, seq_len) = input_ids.dims2()?;
        let mask = if seq_len == 1 {
            None
        } else {
            Some(self.causal_mask(b_sz, seq_len, index_pos)?)
        };

        let mut xs = self.tok_embeddings.forward(input_ids)?;
        // Gemma scales embeddings by sqrt(hidden_size). Dropping this is the
        // classic way to get a model that runs and outputs nonsense.
        xs = (xs * (self.hidden_size as f64).sqrt())?;

        for layer in self.layers.iter_mut() {
            let residual = xs.clone();
            let h = layer.attn_norm.forward(&xs)?;
            let h = layer.forward_attn(&h, mask.as_ref(), index_pos, &self.rotary)?;
            let xs2 = (h + residual)?;

            let residual = xs2.clone();
            let h = layer.ffn_norm.forward(&xs2)?;
            let h = layer.mlp.forward(&h)?;
            xs = (h + residual)?;
        }

        let xs = xs.i((.., seq_len - 1, ..))?;
        let xs = self.norm.forward(&xs)?;
        self.output.forward(&xs)
    }

    /// Drop the KV cache, so the next prompt starts from an empty context.
    pub fn clear_kv_cache(&mut self) {
        for layer in self.layers.iter_mut() {
            layer.kv_cache = None;
        }
    }
}
