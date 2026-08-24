# Releasing @dayone/cli

## Overview

Releases are tag-driven so GitHub Actions and Buildkite build the same commit. GitHub Actions produces Linux and Windows binaries; Buildkite produces signed and notarized macOS binaries. Both upload stripped binaries to one GitHub Release and send matching debug information and source bundles to Sentry. Publishing the release to npm remains a separate manual step.

## Step 1: Build Binaries

1. Confirm the release commit is on `main` and CI is green.
2. Create and push the version tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. Wait for **Build Binaries** in GitHub Actions and both macOS Buildkite jobs to pass.
4. Confirm the GitHub Release contains the seven platform binaries—`dayone-darwin-arm64`, `dayone-darwin-x64`, `dayone-linux-arm64`, `dayone-linux-arm64-no-embeddings`, `dayone-linux-x64`, `dayone-linux-x64-no-embeddings`, and `dayone-win32-x64.exe`—plus `LICENSE` and `LICENSE-NOTICE`.

GitHub Actions and Buildkite may reach the release in either order. Uploads are safe to retry, but npm publishing fails while any required asset is missing.

Embedding availability:

- On Linux with glibc `>= 2.39`, the launcher prefers the standard Linux package (embeddings enabled).
- On older/unknown glibc, the launcher prefers the `-no-embeddings` package for compatibility.
- `-no-embeddings` Linux variants are built on Ubuntu 22.04 to target glibc 2.35.
- The Windows x64 release excludes embeddings so it does not require the VC++ runtime used by fastembed/ONNX. Keyword search remains available.

## Step 2: Publish to npm

1. Go to **Actions** → **Release npm**.
2. Click **Run workflow** and select `main`.
3. Enter the same tag used to build the release (for example, `v0.2.0`). The workflow checks out that tag and derives the package version by stripping the `v` prefix.
4. Leave **dry_run** checked for a first pass, then uncheck it only after validating the candidates.
5. Click **Run workflow**.

This will:

- Check out package source from the supplied tag.
- Download binaries from the GitHub Release for that tag.
- Stop if any supported platform binary is missing.
- Stamp all `package.json` files with the tag version.
- Pack installable candidates during a dry run.
- Publish every platform package and then the main `@dayone/cli` package when `dry_run` is disabled.

All packages are published publicly (`--access public`). Prerelease versions use the npm `next` dist-tag; stable versions use `latest`.

## Versioning

- The tag determines the version in `Cargo.toml`, every `package.json`, and `dayone --version`.
- The `package.json` files in the repository stay at `0.1.0`; the release workflow stamps the published version.
- npm package source and GitHub Release binaries come from the same tag.
- Publishing a prerelease does not move npm's `latest` dist-tag.

## Authentication

- **npm**: Uses GitHub Actions OIDC/trusted publishing (`id-token: write`). Configure every npm package with organization `Automattic`, repository `DayOne-Cli`, and workflow `release-npm.yml`.
- **GitHub Actions release uploads**: Use the built-in `GITHUB_TOKEN`.
- **Buildkite release uploads**: Use GitHub credentials provided by the Automattic CI toolkit.
- **Sentry debug files**: Set `SENTRY_AUTH_TOKEN` for tagged jobs in both GitHub Actions and Buildkite. The token must be authorized to upload debug files to the `a8c/day-one-cli` project. Store it as a secret in both systems; never commit it or pass it as a command-line argument.

Tagged jobs fail before publishing binaries when the Sentry token or debug information is missing. Debug files and source bundles stay in Sentry; they are not attached to GitHub Releases or packed into npm packages. Sentry matches them to events by debug identifier, while the stamped Cargo version supplies the event's release value.

Treat the workflow files as authoritative if this document and CI differ.

## Validate a Release Candidate

1. Push a prerelease tag from the intended release commit (for example, `v0.22.0-rc.1`). Tags containing `-` create a GitHub prerelease.
2. Wait for GitHub Actions and both Buildkite jobs to pass.
3. Confirm the prerelease contains all seven expected assets and that every binary reports the tag version. Run `dayone doctor` for each build and confirm `environment.build_sha` matches the tagged commit rather than `unknown`.
4. On both macOS architectures, verify the signature and notarization requirement:

   ```bash
   codesign --verify --strict --verbose=2 --check-notarization -R='notarized' dayone
   codesign --display --verbose=4 dayone 2>&1
   ```

   `--check-notarization` forces the online ticket lookup required for a bare executable. Confirm the identifier is `com.dayoneapp.cli`, the team identifier is `PZYM8XX95Q`, the authority is Automattic's Developer ID, and the code directory has the `runtime` flag. `spctl --assess --type execute` is not a valid check for a bare command-line executable; it rejects even Apple-signed system tools as “not an app.”
5. Install the downloaded binary using the documented direct-install command and run `dayone --version` outside the download directory.
6. Trigger a controlled, privacy-safe failure on each release target family. Confirm its Sentry event has the stamped release version and symbolicated Rust frames with function, file, line, and source context. Never include journal content or credentials in the test failure.
7. Run **Release npm** with `dry_run` checked. The workflow uploads an `npm-candidates-<version>` artifact containing the packed main package and all platform packages.
8. Download the candidates and test the matching main/platform pair in a temporary npm project:

   ```bash
   mkdir /tmp/dayone-cli-candidate && cd /tmp/dayone-cli-candidate
   npm init -y
   npm install /path/to/dayone-cli-<version>.tgz /path/to/dayone-cli-<platform>-<version>.tgz
   node node_modules/@dayone/cli/run.js --version
   ```

Use an equivalent temporary directory on Windows.

Before publishing publicly:

- Install each supported npm candidate and run the launcher version check.
- Confirm each npm platform binary is byte-for-byte identical to its GitHub Release asset.
- Confirm every npm package contains the GPL license text.
- Upgrade an existing npm install and confirm profiles, authentication, and Keychain access still work.
- Run login, sync, entry create/update/delete, plaintext and end-to-end encrypted entries, and representative media against the release candidate.
- Interrupt and retry a sync; confirm queued changes remain visible with `dayone outbox list`.
- Verify CLI-created encrypted content decrypts in Web and a native Day One client.
- Run every public README example against the same candidate.
- Confirm telemetry opt-out and the documented support/diagnostic commands behave as described.

Attach the results and accepted limitations to the launch-validation issue before unchecking `dry_run`.

After publishing a release candidate, test the real registry path and confirm `latest` still points to the previous stable version:

```bash
npm install -g @dayone/cli@next
dayone --version
npm view @dayone/cli dist-tags --json
```

## Failure Recovery

- Retry failed GitHub Actions or Buildkite jobs against the same tag; Sentry debug uploads are idempotent and release uploads replace only their own named assets.
- If Sentry rejects an upload, verify the token, organization/project slugs, and generated dSYM, ELF debug file, or PDB before retrying.
- Do not move a published tag to a different commit. Fix the problem and create a new prerelease or patch tag.
- Do not run npm publishing until every release asset, macOS signature, notarization requirement, and npm candidate has been verified.
