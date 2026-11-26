#!/usr/bin/env bash
# [DO NOT MERGE] Validation only.
# usage: assert-image.sh v4|v3
# v4's `disable_auto_updates` role writes these Software Update keys as false; v3 has none of them.
set -euo pipefail

expect=$1
keys=(AutomaticDownload AutomaticallyInstallMacOSUpdates ConfigDataInstall CriticalUpdateInstall)

sw_vers
xcodebuild -version

found=0
for key in "${keys[@]}"; do
  value=$(defaults read /Library/Preferences/com.apple.SoftwareUpdate "$key" 2>/dev/null || echo missing)
  echo "SoftwareUpdate $key=$value"
  [ "$value" = 0 ] && found=$((found + 1))
done
echo "commerce AutoUpdate=$(defaults read /Library/Preferences/com.apple.commerce AutoUpdate 2>/dev/null || echo missing)"

case "$expect" in
  v4) [ "$found" -eq "${#keys[@]}" ] || { echo "ASSERTION FAILED: expected all ${#keys[@]} keys off on v4, found $found"; exit 1; } ;;
  v3) [ "$found" -eq 0 ] || { echo "ASSERTION FAILED: expected no keys off on v3, found $found"; exit 1; } ;;
  *) echo "usage: $0 v4|v3"; exit 2 ;;
esac
echo "ASSERTION PASSED: image matches $expect ($found of ${#keys[@]} keys off)"
