---
name: adversarial-review
description: "Run an adversarial code review against the current branch diff for the DayOne CLI Rust codebase. Use when asked to run a review, check code before pushing, or trigger a manual pre-PR review. Runs logic, security, architecture-taste, and test-adversary passes based on risk level."
---

# Adversarial Review

Runs 1–4 adversarial review passes against the current branch diff. Risk level is derived from which files changed. Blocks pushes when P1 issues are found. P2/P3 findings are advisory.

This version is adapted for `dayone-cli`: Rust, clap-style CLI ergonomics, JSON-first output, SQLite/local-first persistence, outbox sync, keychain/SQLite encryption-key storage, and Day One API contract compatibility.

## Passes

| Pass                 | Focus                                                                 |
| -------------------- | --------------------------------------------------------------------- |
| `logic-redteam`      | Bugs, silent failures, data integrity, sync/conflict mistakes         |
| `security`           | command/SQL injection, auth violations, key/PII leakage, crypto misuse |
| `architecture-taste` | CLI layering, API/store boundaries, Rust taste, debt ratchets         |
| `test-adversary`     | Test quality, false positives, overmocking, missing coverage          |

## Risk levels

| Level    | Passes run                                                     |
| -------- | -------------------------------------------------------------- |
| critical | logic-redteam + security + architecture-taste + test-adversary |
| high     | logic-redteam + security + architecture-taste + test-adversary |
| medium   | logic-redteam + architecture-taste + test-adversary            |
| low      | architecture-taste only                                        |

## CI / shell behavior

The hook script `scripts/adversarial-review.sh` uses `set -euo pipefail` and **`trap '' PIPE` immediately after** so truncated diffs (`echo … | head`) do not fail with `echo: write error: Broken pipe` in GitHub Actions or macOS shells. Do not remove that trap without an alternative.

Scripts should remain compatible with macOS default Bash 3.2.

## Manual invocation

To run a review on the current branch without pushing, run from the repo root:

```bash
bash .agents/skills/adversarial-review/scripts/run.sh
```

Requires `claude` CLI and `jq` to be installed and authenticated, unless `ADVERSARIAL_REVIEW_MODEL_COMMAND` is set.

## Output

Findings are written to stderr in `[P1|P2|P3] file:line` format. P1 blocks the push. P2/P3 are advisory — review and address before merge.
