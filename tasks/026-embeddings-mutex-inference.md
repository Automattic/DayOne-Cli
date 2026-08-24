# 026 — Investigate parallel embedding inference (Mutex-serialized ONNX)

**Source:** architecture.md § Embeddings and Semantic Search — critique: Mutex-serialized inference  
**Size:** S  
**Depends on:** nothing (independent investigation; current behaviour is correct, this is a future-proofing task)

---

## Problem

The embedding model (`all-MiniLM-L6-v2`) is held in a `OnceLock<Mutex<TextEmbedding>>`. All embedding calls acquire this mutex for the duration of inference, serializing all embedding work in the process. For the current batch-style usage (embed entries during sync) this is not a practical bottleneck. But if the CLI ever moves toward a daemon or server model that handles multiple concurrent requests, the mutex becomes a significant bottleneck.

The ONNX Runtime supports thread-safe inference sessions in some configurations. If the `fastembed` crate exposes this, embedding calls could run concurrently.

## Goal

Determine whether `fastembed`/ONNX Runtime supports thread-safe inference for the current model configuration. If so, remove the `Mutex` and allow concurrent embedding calls. If not, document why the `Mutex` is necessary and mark this task as resolved-won't-fix until `fastembed` adds support.

## Concrete steps

1. **Research phase:** Check the `fastembed` crate documentation and source for:
   - Whether `TextEmbedding` implements `Send + Sync`.
   - Whether ONNX Runtime sessions are thread-safe for inference (read-only operations should be).
   - Whether `fastembed` exposes a `Clone` or pooling API.

2. If `TextEmbedding` is `Send + Sync` (or can be wrapped in `Arc` without `Mutex`):
   - Replace `OnceLock<Mutex<TextEmbedding>>` with `OnceLock<Arc<TextEmbedding>>` (no mutex).
   - Update `compute_entry_embedding` to acquire a non-locking reference to the model.
   - Verify that concurrent embedding calls in tests produce correct results.

3. If `TextEmbedding` requires serialized access (e.g., it mutates internal state during inference):
   - Confirm this is actually the case by reading the ONNX Runtime and fastembed source.
   - Add a comment to the `Mutex` declaration explaining why it is necessary:
     ```rust
     // Mutex required: fastembed's TextEmbedding mutates internal ONNX session state
     // during inference and is not Send + Sync. Revisit when fastembed adds thread-safe
     // session support.
     static EMBEDDING_MODEL: OnceLock<Mutex<TextEmbedding>> = OnceLock::new();
     ```
   - Close this task as documented-won't-fix.

4. If a model pool approach is viable (e.g., `rayon` thread pool, each thread holding its own `TextEmbedding`):
   - Implement a bounded pool and measure throughput improvement for the batch embedding use case.
   - Only do this if the investigation shows concrete performance benefit.

5. Run `cargo test --locked` after any changes.

## Notes

- This task is low urgency for the current CLI architecture. It is included because the comment in `architecture.md` flags it as a future concern and the investigation is cheap.
- If the investigation takes more than half a day without a clear answer, document findings and close.

## Definition of done

- Either: `Mutex` is removed and concurrent inference is verified.
- Or: A code comment explains why `Mutex` is required and links to the relevant upstream issue or documentation.
- `cargo test --locked` passes.
