#!/usr/bin/env python3
"""Offline checks for release-note baselines and public GitHub API requests."""

import contextlib
import importlib.util
import io
import itertools
import json
from pathlib import Path
import subprocess
import textwrap
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "release_notes", Path(__file__).with_name("release-notes.py")
)
release_notes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_notes)


def release(tag, *, draft=False, prerelease=False):
    return {"tag_name": tag, "draft": draft, "prerelease": prerelease}


class BaselineTests(unittest.TestCase):
    def test_stable_skips_rc_even_if_github_marks_it_stable(self):
        releases = [
            release("v0.21.0"),
            release("v0.22.0-rc.1", prerelease=True),
            release("v0.22.0-rc.2"),
            release("v0.21.1", draft=True),
            release("v0.21.2", prerelease=True),
            release("v0.22.0"),
            release("v0.23.0"),
        ]
        self.assertEqual(release_notes.previous_tag("v0.22.0", releases), "v0.21.0")

    def test_order_and_numeric_version_components(self):
        releases = [release(tag) for tag in ("v1.9.0", "v1.10.0", "v1.2.10", "v1.2.9")]
        for ordering in itertools.permutations(releases):
            self.assertEqual(release_notes.previous_tag("v1.11.0", ordering), "v1.10.0")

    def test_backport_excludes_newer_minor_and_major_versions(self):
        releases = [release(tag) for tag in ("v1.2.8", "v1.3.0", "v2.0.0", "v1.2.9")]
        self.assertEqual(release_notes.previous_tag("v1.2.10", releases), "v1.2.9")

    def test_boundaries_and_build_metadata(self):
        cases = [
            ("v1.0.0", ["v0.99.99", "v1.0.0-rc.99", "v1.0.0"], "v0.99.99"),
            ("v1.10.0", ["v1.9.99", "v1.10.0+build.1"], "v1.9.99"),
            ("v1.0.1+build.2", ["v1.0.0", "v1.0.1+build.1"], "v1.0.0"),
            ("v1.0.1", ["1.0.0", "v1.0.0+build.2", "v1.0.0+build.1"], "v1.0.0+build.2"),
        ]
        for target, tags, expected in cases:
            with self.subTest(target=target):
                self.assertEqual(release_notes.previous_tag(target, map(release, tags)), expected)

    def test_rc_uses_previous_same_channel_in_semantic_order(self):
        releases = [release(tag) for tag in (
            "v1.0.0", "v1.1.0-rc.2", "v1.1.0-rc.10", "v1.1.0-beta.99",
            "v1.1.0-rc.11", "v1.1.0", "v2.0.0-rc.1",
        )]
        self.assertEqual(release_notes.previous_tag("v1.1.0-rc.11", releases), "v1.1.0-rc.10")

    def test_first_rc_falls_back_to_stable(self):
        releases = [release("v1.0.0"), release("v1.1.0-beta.1", prerelease=True)]
        self.assertEqual(release_notes.previous_tag("v1.1.0-rc.1", releases), "v1.0.0")

    def test_prerelease_identifier_precedence(self):
        releases = [release(tag) for tag in (
            "v1.0.0-rc", "v1.0.0-rc.2", "v1.0.0-rc.10", "v1.0.0-rc.a",
        )]
        self.assertEqual(release_notes.previous_tag("v1.0.0-rc.2", releases), "v1.0.0-rc")
        self.assertEqual(release_notes.previous_tag("v1.0.0-rc.a", releases), "v1.0.0-rc.10")
        self.assertEqual(release_notes.previous_tag("v1.0.0-rc.a.1", releases), "v1.0.0-rc.a")

    def test_missing_prior_stable(self):
        releases = [release("v1.0.0-rc.1"), release("v1.0.0"), release("v2.0.0")]
        self.assertIsNone(release_notes.previous_tag("v1.0.0", releases))
        self.assertEqual(release_notes.previous_tag("v1.0.1", releases), "v1.0.0")

    def test_invalid_candidates_are_ignored_and_invalid_target_is_rejected(self):
        releases = [release(tag) for tag in (
            "v1.0.0", "v1.01.0", "v1.1", "v1.1.0-rc.01", "nightly", "v1.1.0\n",
        )]
        self.assertEqual(release_notes.previous_tag("v1.2.0", releases), "v1.0.0")
        for tag in ("v01.0.0", "v1.0.0-01", "v1.0", "v1.0.0\n", "v1.0.0+", "--help"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release_notes.previous_tag(tag, [])


class GenerationTests(unittest.TestCase):
    def run_main(self, tag, handler):
        output = io.StringIO()
        with patch.object(release_notes.subprocess, "run", side_effect=handler), \
                patch("sys.argv", ["release-notes.py", tag]), \
                contextlib.redirect_stdout(output):
            status = release_notes.main()
        return status, output.getvalue()

    def test_public_paginated_requests_and_explicit_baseline(self):
        def gh(command, **kwargs):
            self.assertEqual(command[:4], ["gh", "api", "--hostname", "github.com"])
            endpoint = command[4]
            if endpoint == "repos/Automattic/DayOne-Cli/releases?per_page=100":
                self.assertEqual(command[5:], ["--paginate", "--slurp"])
                response = [[release("v1.10.0-rc.1")], [release("v1.9.0")]]
            elif endpoint == "repos/Automattic/DayOne-Cli/releases/generate-notes":
                self.assertEqual(command[5:], [
                    "--method", "POST", "-f", "tag_name=v1.10.0",
                    "-f", "previous_tag_name=v1.9.0",
                ])
                response = {"body": "## What's Changed\n\nPublic feature (#123)"}
            else:
                self.fail(f"Unexpected endpoint: {endpoint}")
            return subprocess.CompletedProcess(command, 0, json.dumps(response))

        with patch.dict("os.environ", {"GH_REPO": "private/repository", "GH_HOST": "private.example"}):
            self.assertEqual(self.run_main("v1.10.0", gh), (
                0, "## What's Changed\n\nPublic feature (#123)\n",
            ))

    def test_no_prior_stable_links_full_history_not_github_default(self):
        def gh(command, **kwargs):
            self.assertEqual(command[4], "repos/Automattic/DayOne-Cli/releases?per_page=100")
            response = [[release("v1.0.0-rc.1"), release("v1.0.0"), release("v2.0.0")]]
            return subprocess.CompletedProcess(command, 0, json.dumps(response))

        self.assertEqual(self.run_main("v1.0.0", gh), (
            0, "## Changes\n\nNo earlier eligible release is available.\n\n"
            "**Full history**: https://github.com/Automattic/DayOne-Cli/commits/v1.0.0\n",
        ))

    def test_api_failure_does_not_emit_partial_notes(self):
        def gh(command, **kwargs):
            raise subprocess.CalledProcessError(1, command)

        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            self.assertEqual(self.run_main("v1.0.0", gh), (1, ""))
        self.assertIn("Could not generate public release notes:", errors.getvalue())


class ReleaseCreatorTests(unittest.TestCase):
    def test_creation_races_and_retries_preserve_notes(self):
        root = Path(__file__).resolve().parent.parent
        actions = (root / ".github/workflows/build-binaries.yml").read_text()
        buildkite = (root / ".buildkite/commands/build-sign-notarize.sh").read_text()
        # Run only the release commands, never the builds or real gh executable.
        snippets = {
            "actions": textwrap.dedent(actions.split("        run: |\n")[-1]),
            "buildkite": textwrap.dedent(
                "  repo=" + buildkite.split("  repo=", 1)[1].split('  rm -f "dist/$asset_name"', 1)[0]
            ),
        }
        fake_commands = r'''
set -euo pipefail
TAG=v1.1.0
release_tag=$TAG
asset_name=dayone-darwin-arm64
body=absent
uploads=0
generated=0
if [[ "$scenario" == existing ]]; then body='Authored notes'; fi
python3() {
  generated=$((generated + 1))
  if [[ "$scenario" == generation-failure ]]; then return 1; fi
  printf 'Generated notes'
}
gh() {
  case "$1 $2" in
    'release view') [[ "$body" != absent ]] ;;
    'release create')
      if [[ "$scenario" == race ]]; then body='Winner notes'; return 1; fi
      if [[ "$scenario" == creation-failure ]]; then return 1; fi
      body=$(cat "$notes") ;;
    'release upload') uploads=$((uploads + 1)) ;;
    *) return 99 ;;
  esac
}
'''
        for creator, snippet in snippets.items():
            for scenario, expected in (
                ("new", "Generated notes|1|1\n"),
                ("existing", "Authored notes|1|0\n"),
                ("race", "Winner notes|1|1\n"),
                ("generation-failure", ""),
                ("creation-failure", ""),
            ):
                with self.subTest(creator=creator, scenario=scenario):
                    result = subprocess.run(
                        ["bash", "-c", f"scenario={scenario}\n" + fake_commands + snippet
                         + '\nprintf "%s|%s|%s\\n" "$body" "$uploads" "$generated"'],
                        capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode, 1 if scenario.endswith("failure") else 0,
                                     result.stderr)
                    self.assertEqual(result.stdout, expected)


if __name__ == "__main__":
    unittest.main()
