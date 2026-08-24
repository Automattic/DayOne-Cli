# 012 — Fetch feature flags and user profile as a dedicated pre-sync phase

**Source:** architecture.md § The Sync Engine — critique: feature flags and user profile should be a pre-sync phase  
**Size:** M  
**Depends on:** 010 (sync phase split makes this cleaner to implement, but can be done independently)

---

## Problem

Feature flags are currently fetched at step 9 in the pull sequence — well after journals, entries, and other resources have already been pulled. The feature flags then control whether steps 11–13 (journal hierarchy, daily chat, context library) are pulled. This means the pull plan is determined midway through execution rather than upfront.

Two problems result:
1. The first half of the sync runs without knowing whether certain features are enabled. If a feature flag determines that a resource should be pulled, that resource was already skipped.
2. The orchestration logic is coupled to where in the sequence the flags happen to arrive, making it fragile to sequence changes.

## Goal

Fetch `user_profile` and `feature_flags` as a dedicated pre-sync phase before any resource pulls begin. Persist both. Then drive the entire pull plan from the flags. The orchestrator decides *what to pull* before it starts *pulling anything*.

## Concrete steps

1. Extract a `pre_sync_phase` function (in `sync/phases/pull.rs` after task 010, or inline in `engine.rs` for now):
   ```rust
   async fn fetch_pre_sync_resources(
       store: &Store,
       api: &impl DayOneApiClient,
   ) -> anyhow::Result<FeatureFlags> {
       // fetch and persist user_profile
       // fetch and persist feature_flags
       // return parsed FeatureFlags struct
   }
   ```

2. Define a `FeatureFlags` struct (in `src/models/feature_flags.rs` or inline) that captures the feature gate fields used by the sync engine:
   ```rust
   pub struct FeatureFlags {
       pub journal_hierarchy_enabled: bool,
       pub daily_chat_enabled: bool,
       pub context_library_enabled: bool,
   }
   ```
   Parse these from the `feature_flags` JSON row fetched from the server.

3. Move the `user_profile` and `feature_flags` singleton fetches out of the main pull sequence.

4. Call `fetch_pre_sync_resources` at the top of `run_sync`, before the main pull begins:
   ```rust
   let feature_flags = fetch_pre_sync_resources(store, &api).await?;
   // now pull everything, guided by feature_flags
   pull_all_resources(store, &api, ..., &feature_flags).await?;
   ```

5. Update `pull_all_resources` (or the equivalent main pull function) to accept a `&FeatureFlags` parameter and use it to gate steps 11–13 instead of fetching flags inline.

6. Remove the inline feature-flag fetch and the mid-sequence conditional checks that were previously dependent on it.

7. Run `cargo test --locked`. Verify against staging that feature-gated resources (daily chat, context library) are still pulled when the flags are set.

## Notes

- The `feature_flags` singleton should still be upserted to the store so it persists between syncs. But the in-memory `FeatureFlags` struct — not the store read — should drive the pull plan for the current sync run.
- If the pre-sync phase fails (e.g., network error fetching feature flags), sync should abort early rather than proceeding with unknown feature state.

## Definition of done

- `user_profile` and `feature_flags` are fetched before any other resource pull.
- The pull plan is fully determined by `FeatureFlags` before the pull loop starts.
- No inline feature-flag condition checks remain in the middle of the pull sequence.
- `cargo test --locked` passes.
