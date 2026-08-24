#![cfg_attr(not(feature = "embeddings"), allow(dead_code))]

#[cfg(feature = "embeddings")]
use std::time::{Duration, Instant};

#[cfg(feature = "embeddings")]
use anyhow::{Context, Result, anyhow};
#[cfg(feature = "embeddings")]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

#[cfg(feature = "embeddings")]
const DEFAULT_ENTRY_COUNT: usize = 512;
#[cfg(feature = "embeddings")]
const DEFAULT_WORDS_PER_ENTRY: usize = 120;
#[cfg(feature = "embeddings")]
const DEFAULT_BATCH_SIZES: &[usize] = &[1, 2, 4, 8, 16, 32, 64, 128];

#[cfg(feature = "embeddings")]
#[derive(Debug, Clone)]
struct Config {
    entry_count: usize,
    words_per_entry: usize,
    batch_sizes: Vec<usize>,
}

#[cfg(feature = "embeddings")]
#[derive(Debug, Clone)]
struct BenchResult {
    label: String,
    batch_size: usize,
    entries: usize,
    duration: Duration,
    dimensions: usize,
}

#[cfg(feature = "embeddings")]
impl BenchResult {
    fn entries_per_second(&self) -> f64 {
        self.entries as f64 / self.duration.as_secs_f64()
    }

    fn ms_per_entry(&self) -> f64 {
        self.duration.as_secs_f64() * 1_000.0 / self.entries as f64
    }
}

#[cfg(feature = "embeddings")]
fn main() -> Result<()> {
    let config = parse_args()?;
    eprintln!(
        "embedding bench: entries={}, words_per_entry={}, batch_sizes={:?}",
        config.entry_count, config.words_per_entry, config.batch_sizes
    );

    let texts = build_texts(config.entry_count, config.words_per_entry);

    let init_start = Instant::now();
    let mut model = build_model()?;
    let init_duration = init_start.elapsed();
    eprintln!("model init: {:.2}s", init_duration.as_secs_f64());

    // Warm up ONNX/session internals outside the measured runs.
    let warmup = vec![texts[0].clone()];
    let _ = model
        .embed(warmup, Some(1))
        .context("failed warmup embedding")?;

    let sequential = bench_sequential(&mut model, &texts)?;
    print_result(&sequential, None);

    for batch_size in &config.batch_sizes {
        let result = bench_batched(&mut model, &texts, *batch_size)?;
        print_result(&result, Some(&sequential));
    }

    Ok(())
}

#[cfg(not(feature = "embeddings"))]
fn main() {
    eprintln!(
        "embedding_bench requires the 'embeddings' feature. Try: cargo run --release --features embeddings --example embedding_bench"
    );
    std::process::exit(2);
}

#[cfg(feature = "embeddings")]
fn parse_args() -> Result<Config> {
    let mut entry_count = DEFAULT_ENTRY_COUNT;
    let mut words_per_entry = DEFAULT_WORDS_PER_ENTRY;
    let mut batch_sizes = DEFAULT_BATCH_SIZES.to_vec();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--entries" => {
                entry_count = parse_positive_usize("--entries", args.next())?;
            }
            "--words" => {
                words_per_entry = parse_positive_usize("--words", args.next())?;
            }
            "--batch-sizes" => {
                let raw = args
                    .next()
                    .ok_or_else(|| anyhow!("--batch-sizes requires a comma-separated value"))?;
                batch_sizes = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| {
                        value
                            .parse::<usize>()
                            .with_context(|| format!("invalid batch size: {value}"))
                            .and_then(|parsed| {
                                if parsed == 0 {
                                    Err(anyhow!("batch size must be greater than zero"))
                                } else {
                                    Ok(parsed)
                                }
                            })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if batch_sizes.is_empty() {
                    return Err(anyhow!("--batch-sizes must include at least one value"));
                }
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown argument: {other}")),
        }
    }

    Ok(Config {
        entry_count,
        words_per_entry,
        batch_sizes,
    })
}

#[cfg(feature = "embeddings")]
fn parse_positive_usize(flag: &str, value: Option<String>) -> Result<usize> {
    let raw = value.ok_or_else(|| anyhow!("{flag} requires a value"))?;
    let parsed = raw
        .parse::<usize>()
        .with_context(|| format!("invalid {flag} value: {raw}"))?;
    if parsed == 0 {
        return Err(anyhow!("{flag} must be greater than zero"));
    }
    Ok(parsed)
}

#[cfg(feature = "embeddings")]
fn print_help() {
    println!(
        "Usage: cargo run --release --features embeddings --example embedding_bench -- [options]\n\nOptions:\n  --entries N             Number of synthetic entry texts (default: {DEFAULT_ENTRY_COUNT})\n  --words N               Words per synthetic entry (default: {DEFAULT_WORDS_PER_ENTRY})\n  --batch-sizes LIST      Comma-separated batch sizes (default: {})\n  -h, --help              Show this help",
        DEFAULT_BATCH_SIZES
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
}

#[cfg(feature = "embeddings")]
fn build_model() -> Result<TextEmbedding> {
    let options =
        TextInitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(false);
    TextEmbedding::try_new(options).context("failed to initialize all-MiniLM-L6-v2 embedding model")
}

#[cfg(feature = "embeddings")]
fn build_texts(entry_count: usize, words_per_entry: usize) -> Vec<String> {
    const TOPICS: &[&str] = &[
        "hiking", "coffee", "family", "travel", "weather", "journal", "photo", "garden", "music",
        "work", "recipe", "memory", "beach", "mountain", "train", "book",
    ];
    const FILLER: &[&str] = &[
        "morning",
        "quiet",
        "bright",
        "small",
        "remembered",
        "walking",
        "shared",
        "detail",
        "moment",
        "ordinary",
        "later",
        "nearby",
        "simple",
        "window",
        "river",
        "street",
    ];

    (0..entry_count)
        .map(|entry_idx| {
            let topic = TOPICS[entry_idx % TOPICS.len()];
            let mut words = Vec::with_capacity(words_per_entry + 8);
            words.push("passage:".to_owned());
            words.push(format!("Entry {entry_idx} about {topic}."));
            for word_idx in 0..words_per_entry {
                let token = if word_idx % 11 == 0 {
                    topic
                } else {
                    FILLER[(entry_idx + word_idx) % FILLER.len()]
                };
                words.push(token.to_owned());
            }
            words.join(" ")
        })
        .collect()
}

#[cfg(feature = "embeddings")]
fn bench_sequential(model: &mut TextEmbedding, texts: &[String]) -> Result<BenchResult> {
    let start = Instant::now();
    let mut dimensions = 0;
    for text in texts {
        let embeddings = model
            .embed(vec![text.clone()], Some(1))
            .context("failed sequential embedding")?;
        dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
    }
    Ok(BenchResult {
        label: "sequential".to_owned(),
        batch_size: 1,
        entries: texts.len(),
        duration: start.elapsed(),
        dimensions,
    })
}

#[cfg(feature = "embeddings")]
fn bench_batched(
    model: &mut TextEmbedding,
    texts: &[String],
    batch_size: usize,
) -> Result<BenchResult> {
    let start = Instant::now();
    let mut dimensions = 0;
    let mut embedded = 0;
    for chunk in texts.chunks(batch_size) {
        let embeddings = model
            .embed(chunk.to_vec(), Some(batch_size))
            .with_context(|| format!("failed batched embedding for batch size {batch_size}"))?;
        if embeddings.len() != chunk.len() {
            return Err(anyhow!(
                "batch size {batch_size} returned {} embeddings for {} inputs",
                embeddings.len(),
                chunk.len()
            ));
        }
        dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
        embedded += embeddings.len();
    }
    Ok(BenchResult {
        label: "batched".to_owned(),
        batch_size,
        entries: embedded,
        duration: start.elapsed(),
        dimensions,
    })
}

#[cfg(feature = "embeddings")]
fn print_result(result: &BenchResult, baseline: Option<&BenchResult>) {
    let speedup = baseline
        .map(|baseline| baseline.duration.as_secs_f64() / result.duration.as_secs_f64())
        .unwrap_or(1.0);
    println!(
        "{:<10} batch={:<4} entries={:<5} dims={:<4} total={:>8.3}s  {:>8.2} entries/s  {:>7.2} ms/entry  speedup={:>5.2}x",
        result.label,
        result.batch_size,
        result.entries,
        result.dimensions,
        result.duration.as_secs_f64(),
        result.entries_per_second(),
        result.ms_per_entry(),
        speedup,
    );
}
