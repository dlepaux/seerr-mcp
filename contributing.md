# Contributing

Thanks for helping with seerr-mcp.

## Getting started

```bash
git clone https://github.com/YOUR_USERNAME/seerr-mcp.git
cd seerr-mcp
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`rust-toolchain.toml` pins the compiler, and rustup installs it on the first `cargo` call. Seerr and
Hermes are wiremock servers inside the tests, so the suite needs nothing running.

## Issues

Search [existing issues](https://github.com/dlepaux/seerr-mcp/issues) first. One bug or one request
per issue.

- **Bug:** what you did, what you expected, what happened, and your versions of seerr-mcp, Seerr and
  the chat client. Redact tokens, keys and secrets from anything you paste.
- **Feature:** what you want to do and why.
- **Language:** a new one is welcome, and needs no Rust beyond one line. See the readme's
  [Add a language](readme.md#add-a-language).

A security problem goes through [security.md](security.md), never a public issue.

## Pull requests

- Target `main`.
- Add a test for every behaviour you change. A fix comes with the test that failed before it.
- Keep `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` green.
- Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:` and so
  on). semantic-release reads them to pick the next version and write [changelog.md](changelog.md).
- A new failure path increments a `_failures_total` or `_skipped_total` counter with a `kind` label,
  as the existing ones do, and the readme's metrics table lists it.

## Code of conduct

This project follows its [code of conduct](code-of-conduct.md).
