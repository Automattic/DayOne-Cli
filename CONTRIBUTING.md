# Contributing to Day One CLI

Thank you for contributing.

## Before you start

- Report security vulnerabilities privately as described in [SECURITY.md](SECURITY.md).
- Use [Day One Support](https://dayone.me/support) for account-specific problems. Do not post journal content, credentials, encryption keys, or other private account data in a public issue.
- Open an issue before starting a large behavior or interface change so maintainers can confirm the intended scope.

## Development setup

Install the stable Rust toolchain and the media tools used by attachment tests (`ffmpeg` and `ffprobe`). Then run:

```bash
cargo build
cargo test --locked
cargo fmt -- --check
cargo clippy --locked
```

The test suite is self-contained and must not require a Day One account or network access.

## Pull requests

Keep each pull request focused. Include:

- A clear description of the behavior change.
- Tests for non-trivial logic and bug fixes.
- Documentation updates when commands, output, configuration, or supported behavior changes.
- The validation commands you ran.

Do not commit generated binaries, local configuration, authentication state, screenshots containing account data, or real credentials disguised as test fixtures.

By contributing, you agree that your contribution is licensed under the repository's [GPL-2.0-or-later terms](LICENSE-NOTICE).
