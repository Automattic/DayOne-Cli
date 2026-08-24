#!/bin/bash
# adversarial-review.sh — Pre-push entry point for the adversarial-review skill
#
# Reads hook JSON from stdin, runs adversarial review passes against the branch
# diff, and blocks the push on P1 findings.

set -euo pipefail
# Ignore SIGPIPE from writers when a pipe consumer exits early (e.g. echo … | head).
# Keeps strict mode without treating broken pipe as failure.
trap '' PIPE

SKILL_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/review-core.sh
source "$SKILL_DIR/scripts/lib/review-core.sh"

input=$(cat)
if ! command -v jq &> /dev/null; then
  echo "Error: 'jq' is required for the adversarial review hook. Please install it." >&2
  exit 1
fi
cwd=$(echo "$input" | jq -r '.cwd // empty')
if [[ -z "$cwd" ]]; then
  cwd="$(pwd)"
fi
cd "$cwd"

BASE_REF="${ADVERSARIAL_REVIEW_BASE_REF:-origin/main}"
BASE=$(git merge-base HEAD "$BASE_REF" 2>/dev/null || echo "$BASE_REF")
CHANGED_FILES=$(git diff --name-only "$BASE"..HEAD 2>/dev/null || echo "")

if [[ -z "$CHANGED_FILES" ]]; then
  exit 0
fi

DIFF=$(git diff "$BASE"..HEAD)
DIFF_STAT=$(git diff --stat "$BASE"..HEAD)
LINE_COUNT=$(echo "$DIFF" | wc -l | tr -d ' ')

# Skip trivial diffs.
if [[ "$LINE_COUNT" -lt 10 ]]; then
  exit 0
fi

assess_file_path_risk
load_system_understanding
load_taste
select_passes
run_passes "$DIFF" "$CHANGED_FILES" "$DIFF_STAT"
report_findings

exit 0
