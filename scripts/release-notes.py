#!/usr/bin/env python3
"""Print public release notes without relying on GitHub's latest-release order."""

import argparse
import json
import re
import subprocess
import sys
from urllib.parse import quote


REPOSITORY = "Automattic/DayOne-Cli"
VERSION = re.compile(
    r"v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)


def version_key(tag):
    """Return SemVer precedence, ignoring build metadata; reject invalid tags."""
    match = VERSION.fullmatch(tag)
    if not match:
        raise ValueError(f"invalid semantic version tag: {tag}")
    identifiers = (match[4] or "").split(".") if match[4] else []
    for identifier in identifiers:
        if identifier.isdigit() and len(identifier) > 1 and identifier[0] == "0":
            raise ValueError(f"invalid numeric prerelease identifier: {tag}")
    prerelease = tuple(
        (0, int(identifier)) if identifier.isdigit() else (1, identifier)
        for identifier in identifiers
    )
    return (int(match[1]), int(match[2]), int(match[3]), not identifiers, prerelease)


def previous_tag(tag, releases):
    """Choose a lower published version, not a release-list or creation order."""
    current = version_key(tag)
    stable = []
    same_channel = []
    for release in releases:
        if release["draft"]:
            continue
        candidate = release["tag_name"]
        try:
            version = version_key(candidate)
        except ValueError:
            continue
        if version >= current:
            continue
        if version[3] and not release["prerelease"]:
            stable.append((version, candidate))
        elif not current[3] and not version[3] and version[4][0] == current[4][0]:
            same_channel.append((version, candidate))
    # RCs compare with earlier RCs, then fall back to the previous stable.
    # The tag breaks equal-precedence ties deterministically, including +build tags.
    eligible = same_channel or stable
    return max(eligible)[1] if eligible else None


def github_api(*arguments):
    result = subprocess.run(
        ["gh", "api", "--hostname", "github.com", *arguments],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def generate_notes(tag):
    version_key(tag)  # Fail before making a request for an invalid release tag.
    pages = github_api(
        f"repos/{REPOSITORY}/releases?per_page=100", "--paginate", "--slurp"
    )
    baseline = previous_tag(tag, [release for page in pages for release in page])
    if baseline is None:
        # GitHub's implicit baseline could be an RC or a newer backport release.
        # With no eligible baseline, link the complete public history instead.
        history = f"https://github.com/{REPOSITORY}/commits/{quote(tag, safe='')}"
        return f"## Changes\n\nNo earlier eligible release is available.\n\n**Full history**: {history}\n"
    response = github_api(
        f"repos/{REPOSITORY}/releases/generate-notes",
        "--method", "POST",
        "-f", f"tag_name={tag}",
        "-f", f"previous_tag_name={baseline}",
    )
    return response["body"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", help="release tag, for example v0.22.0")
    args = parser.parse_args()
    try:
        notes = generate_notes(args.tag)
    except (ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"Could not generate public release notes: {error}", file=sys.stderr)
        return 1
    print(notes, end="" if notes.endswith("\n") else "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
