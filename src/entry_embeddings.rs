#[cfg(test)]
use std::cell::RefCell;
#[cfg(all(not(test), feature = "embeddings"))]
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
#[cfg(all(not(test), feature = "embeddings"))]
use anyhow::{Context, anyhow};
#[cfg(all(not(test), feature = "embeddings"))]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const ENTRY_EMBEDDING_DIMENSION: usize = 384;

#[cfg(all(not(test), feature = "embeddings"))]
static EMBEDDING_MODEL: OnceLock<Result<Mutex<TextEmbedding>, String>> = OnceLock::new();

pub const fn embeddings_enabled() -> bool {
    cfg!(any(test, feature = "embeddings"))
}

pub fn source_hash_for_searchable_text(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    Some(sha256_hex(text))
}

pub fn compute_embedding_for_searchable_text(text: &str) -> Result<Option<Vec<f64>>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let embedding = embed_passage_text(text)?;
    Ok(Some(embedding))
}

pub fn compute_embeddings_for_searchable_texts(texts: &[String]) -> Result<Vec<Vec<f64>>> {
    embed_passage_texts(texts)
}

pub fn query_embedding(query: &str) -> Result<Vec<f64>> {
    embed_query_text(query)
}

pub fn entry_searchable_text(record: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for value in [
        record.get("title").and_then(Value::as_str),
        record.get("body").and_then(Value::as_str),
        record
            .get("payload")
            .and_then(|v| v.get("body"))
            .and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_owned());
        }
    }
    parts.join("\n")
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    format!("{:x}", digest)
}

#[cfg(all(not(test), feature = "embeddings"))]
fn embed_passage_text(input: &str) -> Result<Vec<f64>> {
    embed_with_prefix("passage", input)
}

#[cfg(all(not(test), feature = "embeddings"))]
fn embed_passage_texts(inputs: &[String]) -> Result<Vec<Vec<f64>>> {
    embed_with_prefix_batch("passage", inputs, inputs.len())
}

#[cfg(all(not(test), feature = "embeddings"))]
fn embed_query_text(input: &str) -> Result<Vec<f64>> {
    embed_with_prefix("query", input)
}

#[cfg(all(not(test), feature = "embeddings"))]
fn embed_with_prefix(prefix: &str, input: &str) -> Result<Vec<f64>> {
    let embeddings = embed_with_prefix_batch(prefix, &[input.to_owned()], 1)?;
    embeddings
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("fastembed returned no embeddings"))
}

#[cfg(all(not(test), feature = "embeddings"))]
fn embed_with_prefix_batch(
    prefix: &str,
    inputs: &[String],
    batch_size: usize,
) -> Result<Vec<Vec<f64>>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let mut model = embedding_model()?
        .lock()
        .map_err(|_| anyhow!("embedding model mutex poisoned"))?;
    let formatted = inputs
        .iter()
        .map(|input| format!("{prefix}: {input}"))
        .collect::<Vec<_>>();
    let embeddings = model
        .embed(formatted, Some(batch_size))
        .context("failed to generate embeddings with fastembed")?;
    if embeddings.len() != inputs.len() {
        return Err(anyhow!(
            "fastembed returned {} embeddings for {} inputs",
            embeddings.len(),
            inputs.len()
        ));
    }
    embeddings
        .into_iter()
        .map(|embedding| {
            if embedding.len() != ENTRY_EMBEDDING_DIMENSION {
                return Err(anyhow!(
                    "unexpected embedding dimension: got {}, expected {}",
                    embedding.len(),
                    ENTRY_EMBEDDING_DIMENSION
                ));
            }
            Ok(embedding.into_iter().map(f64::from).collect())
        })
        .collect()
}

#[cfg(all(not(test), feature = "embeddings"))]
fn embedding_model() -> Result<&'static Mutex<TextEmbedding>> {
    let init = EMBEDDING_MODEL.get_or_init(|| {
        let options =
            TextInitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(false);
        TextEmbedding::try_new(options)
            .map(Mutex::new)
            .map_err(|err| format!("{err:#}"))
    });
    match init {
        Ok(model) => Ok(model),
        Err(err) => Err(anyhow!(
            "failed to initialize all-MiniLM-L6-v2 embedding model: {err}"
        )),
    }
}

#[cfg(all(not(test), not(feature = "embeddings")))]
fn embed_passage_text(_input: &str) -> Result<Vec<f64>> {
    Err(anyhow::anyhow!(
        "embeddings are disabled in this build (compiled without the 'embeddings' feature)"
    ))
}

#[cfg(all(not(test), not(feature = "embeddings")))]
fn embed_passage_texts(_inputs: &[String]) -> Result<Vec<Vec<f64>>> {
    Err(anyhow::anyhow!(
        "embeddings are disabled in this build (compiled without the 'embeddings' feature)"
    ))
}

#[cfg(all(not(test), not(feature = "embeddings")))]
fn embed_query_text(_input: &str) -> Result<Vec<f64>> {
    Err(anyhow::anyhow!(
        "semantic query embeddings are disabled in this build (compiled without the 'embeddings' feature)"
    ))
}

#[cfg(test)]
fn embed_passage_text(input: &str) -> Result<Vec<f64>> {
    let embeddings = embed_passage_texts(&[input.to_owned()])?;
    embeddings
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("test embedding returned no embeddings"))
}

#[cfg(test)]
fn embed_passage_texts(inputs: &[String]) -> Result<Vec<Vec<f64>>> {
    record_test_embedding_batch_size(inputs.len());
    inputs
        .iter()
        .map(|input| {
            if input.contains("<<force-embedding-failure>>") {
                return Err(anyhow::anyhow!(
                    "forced embedding failure for tests via marker"
                ));
            }
            Ok(embed_text_deterministic(input, ENTRY_EMBEDDING_DIMENSION))
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn with_test_embedding_batch_sizes<T>(f: impl FnOnce() -> T) -> (T, Vec<usize>) {
    struct RecorderGuard {
        previous: Option<Vec<usize>>,
        active: bool,
    }

    impl Drop for RecorderGuard {
        fn drop(&mut self) {
            if self.active {
                TEST_EMBEDDING_BATCH_SIZES.with(|slot| {
                    slot.replace(self.previous.take());
                });
            }
        }
    }

    let previous = TEST_EMBEDDING_BATCH_SIZES.with(|slot| slot.replace(Some(Vec::new())));
    let mut guard = RecorderGuard {
        previous,
        active: true,
    };
    let result = f();
    let sizes = TEST_EMBEDDING_BATCH_SIZES.with(|slot| {
        slot.replace(guard.previous.take())
            .expect("test embedding batch size recorder should be active")
    });
    guard.active = false;
    (result, sizes)
}

#[cfg(test)]
fn record_test_embedding_batch_size(size: usize) {
    TEST_EMBEDDING_BATCH_SIZES.with(|slot| {
        if let Some(sizes) = slot.borrow_mut().as_mut() {
            sizes.push(size);
        }
    });
}

#[cfg(test)]
thread_local! {
    static TEST_EMBEDDING_BATCH_SIZES: RefCell<Option<Vec<usize>>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn embed_query_text(input: &str) -> Result<Vec<f64>> {
    Ok(embed_text_deterministic(input, ENTRY_EMBEDDING_DIMENSION))
}

#[cfg(test)]
fn embed_text_deterministic(input: &str, dimension: usize) -> Vec<f64> {
    if dimension == 0 {
        return Vec::new();
    }
    let mut counts = std::collections::HashMap::<String, f64>::new();
    for token in tokenize(input) {
        *counts.entry(token).or_insert(0.0) += 1.0;
    }
    let mut embedding = vec![0.0_f64; dimension];
    for (token, count) in counts {
        let idx = hash_token_to_index(&token, dimension);
        embedding[idx] += count;
    }
    normalize_vector(&mut embedding);
    embedding
}

#[cfg(test)]
fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

#[cfg(test)]
fn hash_token_to_index(token: &str, size: usize) -> usize {
    let mut hash = 2166136261u32;
    for b in token.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    (hash as usize) % size
}

#[cfg(test)]
fn normalize_vector(values: &mut [f64]) {
    let norm = values.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm == 0.0 {
        return;
    }
    for value in values {
        *value /= norm;
    }
}
