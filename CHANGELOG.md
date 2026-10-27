# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Versions before
0.5.0 predate this changelog.

## [Unreleased]

### Added

- `SHOW CREATE TABLE` output is no longer cell-truncated on the terminal — the DDL is the
  deliverable and now arrives in one step (`roq schema` included). An explicit `--max-cell`
  still takes precedence over the exemption.
- When a tsv cell is truncated, stderr now states it explicitly and points to the archived
  full copy (suppressed by `--quiet`, like other notices).

### Fixed

- Post-0.10.0 quality pass: config section replacement and `migrate` semantics, input
  validation, atomic file writes.

### Documentation

- README rewritten in English with a standardized structure (highlights → installation →
  quick start → usage → configuration → security model → audit); this changelog introduced.
- Help text covers the `config` and `log` subcommand entry points; `diff` line mentions
  `--sample`; local update instructions switched to `cargo install`.

## [0.10.0] — 2026-10-23

### Added

- Windows: `config add --password` encrypts at rest with DPAPI (`enc:v1:dpapi:` prefix, bound
  to the local Windows user); read side decrypts and verifies before connecting.
- `config add --update` re-records an existing profile in place (re-entering a password
  re-encrypts); duplicate section names across files are pre-checked; `0600` permissions on
  Unix.
- `roq config migrate`: converts legacy plaintext passwords to DPAPI ciphertext in place —
  decrypts to verify before writing, touches only `password=` lines, idempotent.
- Plaintext-password detection warns on load; non-Windows platforms reject `--password` and
  point to `--password-env` (fails fast when the variable is unset — no plaintext fallback).
- CI: `cargo test` gates every release build — no artifacts ship from a red suite.

## [0.9.0] — 2026-10-12

### Added

- `roq diff --sample N`: on mismatch, show rows present on only one side (default 5 per side,
  `0` disables); `--config` passthrough; type-aware row digests (numeric `1` ≠ text `'1'`).
- `roq columns <table>`: `SHOW FULL COLUMNS` shortcut — fields, types, and comments, more
  compact than full DDL.

### Fixed

- Windows: switch the console output code page to UTF-8 at startup to fix mojibake in conhost.

## [0.8.0] — 2026-10-10

### Added

- `roq diff`: run one SQL on two profiles and reconcile — ordered column names, row counts,
  and an order-insensitive, duplicate-sensitive multiset digest; exit code `4` on mismatch.
- `--list` shows each profile's `host:port` and database name.
- JSON output gains a top-level `elapsedMs`.

### Fixed

- Included in this release: `ssl=true` never took effect (the MySQL client's default features
  lacked a TLS backend) — fixed after 0.7.0 as 0.7.1, never tagged separately.

## [0.7.0] — 2026-10-09

### Added

- `roq skill install [--global|--project|--codex] [--force]`: the AI usage skill
  (`skill/roq/SKILL.md`) ships inside the binary and installs into Claude Code or Codex CLI —
  same version as the binary, no machine-local paths.
- Release pipeline: pushing a `v*` tag builds and publishes three-platform archives (binary +
  skill) to GitHub Releases.

## [0.6.0] — 2026-10-07

### Added

- Shortcut subcommands: `roq tables` / `roq schema <table>` / `roq explain "SQL"` — they only
  generate SQL and pass through the same gate + audit + archive pipeline.
- `roq log --last N`: scan across monthly log files.

### Fixed

- Unknown flags now produce a usage error instead of being silently folded into the SQL.

## [0.5.0] — 2026-10-07

### Added

- Output formats `--format tsv|json|csv` (json/csv are interchange formats — cells never
  truncated).
- `--out FILE`: write results to a file instead of the terminal; `--quiet` suppresses stderr
  notices.

*Earlier versions (the read-only gate, profiles, audit log, result archive, `roq log`,
`password_env`) predate this changelog.*
