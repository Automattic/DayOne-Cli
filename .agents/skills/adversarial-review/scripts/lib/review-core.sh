#!/bin/bash
# review-core.sh — Shared logic for adversarial-review.sh
#
# Caller must set:
#   SKILL_DIR — absolute path to .agents/skills/adversarial-review
#   CHANGED_FILES — space/newline-separated list of changed files
#
# Functions set these vars:
#   RISK, SYSTEM_UNDERSTANDING, TASTE, PASSES, ALL_FINDINGS, HAS_P1

promote_to() {
  local target="$1"
  case "$target" in
    critical) RISK="critical" ;;
    high)
      if [[ "$RISK" != "critical" ]]; then RISK="high"; fi ;;
    medium)
      if [[ "$RISK" == "low" ]]; then RISK="medium"; fi ;;
  esac
}

inject_section() {
  local key="$1"
  local file="$SKILL_DIR/references/system-understanding/${key}.md"
  case "$INJECTED_SECTIONS" in
    *" $key "*|*" $key") return ;;
  esac
  INJECTED_SECTIONS="$INJECTED_SECTIONS $key"
  if [[ -f "$file" ]]; then
    SYSTEM_UNDERSTANDING+=$(cat "$file")$'\n\n'
  fi
}

assess_file_path_risk() {
  if [[ -n "${ADVERSARIAL_REVIEW_FORCE_RISK:-}" ]]; then
    RISK="$ADVERSARIAL_REVIEW_FORCE_RISK"
    return
  fi

  RISK="low"
  for file in $CHANGED_FILES; do
    case "$file" in
      src/sync/crypto.rs|src/store/encryption.rs|src/store/migrations/*|src/store/sqlite.rs|src/sync/engine.rs|src/sync/phases/*|src/entry/body.rs|src/entry/rich_text.rs|src/entry/attachments.rs)
        promote_to "critical" ;;
      src/http/*|src/config.rs|src/env_util.rs|src/commands/auth_*.rs|src/commands/sync*.rs|src/commands/entry_*.rs|src/commands/comment*.rs|src/commands/journal_*.rs|src/commands/context_item_*.rs|src/commands/daily_chat_*.rs|src/commands/memory_process.rs|src/comment_codec.rs|src/models/*|src/store/*|src/sync/*|src/telemetry/*)
        promote_to "high" ;;
      src/cli/*|src/commands/list*.rs|src/commands/search*.rs|src/commands/user_settings_*.rs|src/commands/profile.rs|src/commands/embeddings*.rs|src/convert/*|src/tui/*|src/http/fake.rs|fixtures/*|e2e/*)
        promote_to "medium" ;;
      tests/*|docs/*|.agents/*|.pi/*|githooks/*|*.md|Cargo.toml|Cargo.lock)
        ;; # stays at current level
      *)
        ;; # unknown files stay low unless another file promotes
    esac
  done
}

load_system_understanding() {
  SYSTEM_UNDERSTANDING=""
  INJECTED_SECTIONS=""
  local su_dir="$SKILL_DIR/references/system-understanding"

  if [[ -d "$su_dir" ]]; then
    for file in $CHANGED_FILES; do
      case "$file" in
        src/sync/crypto.rs|src/store/encryption.rs|*crypto*|*encryption*)
          inject_section "crypto" ;;
      esac
      case "$file" in
        src/sync/*|src/commands/sync*.rs|*outbox*)
          inject_section "sync" ;;
      esac
      case "$file" in
        src/store/*|src/store/migrations/*)
          inject_section "sqlite-store" ;;
      esac
      case "$file" in
        src/http/*|src/config.rs|src/env_util.rs|src/commands/auth_*.rs|src/commands/profile.rs|src/commands/user_settings_*.rs)
          inject_section "http-auth-config" ;;
      esac
      case "$file" in
        src/entry/*|src/convert/*|*rich_text*|*attachment*|*moment*)
          inject_section "entry-rich-text-media" ;;
      esac
      case "$file" in
        src/commands/daily_chat_*.rs|src/commands/memory_process.rs|*daily_chat*|*memory*|*context_item*)
          inject_section "daily-chat-memory" ;;
      esac
      case "$file" in
        src/commands/comment*.rs|src/comment_codec.rs|*comment*|*journal_create*|*shared*)
          inject_section "comments-shared" ;;
      esac
      case "$file" in
        src/telemetry/*|*sentry*|*telemetry*)
          inject_section "telemetry" ;;
      esac
      case "$file" in
        src/cli/*|src/commands/*.rs|src/tui/*)
          inject_section "cli-ux" ;;
      esac
      case "$file" in
        tests/*|e2e/*|*test*)
          inject_section "rust-testing" ;;
      esac
    done
  fi

  if [[ -z "$SYSTEM_UNDERSTANDING" ]]; then
    SYSTEM_UNDERSTANDING="No area-specific system understanding applies to this diff."
  fi
}

load_taste() {
  TASTE=""
  local taste_file="$SKILL_DIR/references/code-taste.md"
  if [[ -f "$taste_file" ]]; then
    TASTE=$(cat "$taste_file")
  fi
}

select_passes() {
  PASSES=()
  case "$RISK" in
    critical|high)
      PASSES=("logic-redteam" "security" "architecture-taste" "test-adversary")
      ;;
    medium)
      PASSES=("logic-redteam" "architecture-taste" "test-adversary")
      ;;
    low)
      PASSES=("architecture-taste")
      ;;
  esac
}

run_passes() {
  local diff="$1"
  local changed_files="$2"
  local diff_stat="$3"
  local prompts_dir="$SKILL_DIR/references/prompts"

  ALL_FINDINGS=""
  HAS_P1=false

  local line_count
  line_count=$(echo "$diff" | wc -l | tr -d ' ')

  echo "Adversarial review: risk=$RISK, passes=${#PASSES[@]}, diff=${line_count} lines" >&2
  echo "$diff_stat" >&2
  echo "" >&2

  for pass in "${PASSES[@]}"; do
    local prompt_file="$prompts_dir/${pass}.md"
    if [[ ! -f "$prompt_file" ]]; then
      echo "Warning: prompt file not found: $prompt_file" >&2
      continue
    fi

    local prompt
    prompt=$(cat "$prompt_file")
    prompt="${prompt//\{\{TASTE\}\}/$TASTE}"
    prompt="${prompt//\{\{SYSTEM_UNDERSTANDING\}\}/$SYSTEM_UNDERSTANDING}"

    local full_prompt="$prompt

## Diff to review

Changed files:
$changed_files

\`\`\`diff
$diff
\`\`\`"

    echo "Running pass: $pass..." >&2

    local result
    if [[ -n "${ADVERSARIAL_REVIEW_MODEL_COMMAND:-}" ]]; then
      result=$(printf '%s' "$full_prompt" | bash -c "$ADVERSARIAL_REVIEW_MODEL_COMMAND" || echo "REVIEW_PASS_FAILED")
    else
      result=$(printf '%s' "$full_prompt" | claude --print --model claude-opus-4-6 || echo "REVIEW_PASS_FAILED")
    fi

    if [[ "$result" == "REVIEW_PASS_FAILED" ]]; then
      echo "  Warning: $pass pass failed to run" >&2
      continue
    fi

    if [[ "$result" != *"NO_ISSUES_FOUND"* ]]; then
      ALL_FINDINGS+="### $pass"$'\n'"$result"$'\n\n'
      if echo "$result" | grep -q '\[P1\]'; then
        HAS_P1=true
      fi
    else
      echo "  $pass: clean" >&2
    fi
  done
}

report_findings() {
  local block_message="${1:-PUSH BLOCKED: P1 issues found. Fix the P1 findings above and retry.}"

  if [[ -n "$ALL_FINDINGS" ]]; then
    echo "" >&2
    echo "═══════════════════════════════════════════" >&2
    echo "  ADVERSARIAL REVIEW FINDINGS" >&2
    echo "  Risk level: $RISK" >&2
    echo "═══════════════════════════════════════════" >&2
    echo "" >&2
    echo "$ALL_FINDINGS" >&2

    if $HAS_P1; then
      echo "═══════════════════════════════════════════" >&2
      echo "  $block_message" >&2
      echo "═══════════════════════════════════════════" >&2
      exit 2
    else
      echo "═══════════════════════════════════════════" >&2
      echo "  Advisory: P2/P3 issues found (not blocking)." >&2
      echo "  Consider addressing before merge." >&2
      echo "═══════════════════════════════════════════" >&2
    fi
  fi
}
