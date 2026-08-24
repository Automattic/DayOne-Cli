#!/bin/bash
# run.sh — Manually trigger adversarial review on the current branch.
# Run from the repo root: bash .agents/skills/adversarial-review/scripts/run.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
echo "{\"cwd\": \"$(pwd)\"}" | bash "$SCRIPT_DIR/adversarial-review.sh"
