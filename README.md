# roq

> **A read-only MySQL client you can point at production without fear.**

Read this in: [简体中文](README.zh-CN.md)

[![Release](https://img.shields.io/github/v/release/Alleyway-boop/roq)](https://github.com/Alleyway-boop/roq/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

`roq` is a command-line query tool for MySQL in which write operations are impossible — not by
policy, but by construction. Three independent read-only layers stand between your terminal and
the database, every invocation is audited and archived, and the whole thing ships as a single
~2.5 MB static binary with no runtime dependencies.

It is built for incident investigation and data verification on production, staging, and
development databases — used by humans directly, and by AI agents through a bundled skill.

> Note: interactive CLI messages (help, warnings, audit summaries) are in Chinese. Query output
> data is unaffected and fully UTF-8 clean.

## Highlights

- **Read-only by construction** — a local statement gate, a server-side `READ ONLY` session
  (verified by read-back), and output caps. Even rejected or failed attempts leave an audit trail.
- **Audited and archived** — every invocation is logged to a monthly JSONL audit log; full results
  are always archived to disk, even when the terminal display is truncated.
- **Agent-ready** — `roq skill install` teaches Claude Code / Codex CLI how to use roq correctly
  in any project; `--format json --quiet` gives agents a single clean machine-readable document.
- **Credentials protected at rest** — Windows DPAPI encryption bound to the local user;
  environment-variable credentials on Linux/macOS/CI.
- **Reconciliation built in** — `roq diff` runs one SQL statement on two profiles and reports
  mismatches with sample diff rows.
- **Named saved queries** — `roq q` stores recurring SQL as templates with `{{placeholder}}`
  parameters; call them by name with `k=v` arguments and they run through the same read-only
  gate and audit trail.
- **Single static binary** — written in Rust, ~2.5 MB, zero runtime dependencies.

## Installation

Pick one:

1. **Download a release** (no Rust toolchain needed): grab the archive for your platform from
   [GitHub Releases](https://github.com/Alleyway-boop/roq/releases) — `windows-x64`,
   `macos-arm64`, `macos-x64`, or `linux-x64` — and unpack it. You get the `roq` binary plus the
   `skill/` directory.
2. **Install with cargo**: `cargo install --git https://github.com/Alleyway-boop/roq`
3. **Build from source**: `git clone` this repository, then `cargo build --release`.

Add the binary's directory to your `PATH`, then (optionally) install the AI skill so agents in
any project use roq the right way:

```bash
roq skill install --global    # Claude Code: ~/.claude/skills/roq/
roq skill install --codex     # OpenAI Codex CLI: ~/.codex/skills/roq/
```

The skill ships inside the binary, so its version always matches. After upgrading roq, re-run
`roq skill install --force` to sync it.

## Quick start

```bash
# 1. Register a database (one file per profile under ~/.roq/profiles.d/)
roq config add myproj-prod --host db.example.com --user roq_ro --password '***' --database mydb

# 2. Verify the connection (connects, asserts read-only, probes server version)
roq config test myproj-prod

# 3. Look around, then query
roq tables --profile myproj-prod
roq tables mark --profile myproj-prod              # fuzzy: bare substring, auto-wrapped in %
roq columns t_order --profile myproj-prod          # fields, types, comments
roq --profile myproj-prod --format json --quiet "SELECT COUNT(*) AS cnt FROM t_order"
```

On Windows, the password is encrypted with DPAPI the moment it is written to disk. Naming a
production profile `prod` (or anything containing `prod`) triggers an explicit reminder on every
query.

## Usage

```
roq [--profile NAME] [--format tsv|json|csv] [--max-rows N] [--max-cell N]
    [--out FILE] [--quiet] [--config PATH] [--list] "SINGLE READ-ONLY SQL"
roq tables [PATTERN] [flags]          # SHOW TABLES (substring auto-wrapped in %; explicit % used verbatim)
roq schema <table|db.table> [flags]   # SHOW CREATE TABLE (strict name validation)
roq columns <table|db.table> [PATTERN] [flags]  # SHOW FULL COLUMNS (fields, types, comments; PATTERN filters)
roq explain "SQL" [flags]             # prefixes EXPLAIN
roq q <name> [k=v ...] [flags]        # run a saved query by name (params fill {{placeholders}})
roq q add|list|show|remove ...        # manage saved queries (details: roq q --help)
roq diff --profile LEFT --profile RIGHT "SQL" [--json] [--sample N] [flags]
roq config add|test|remove|list|migrate ...
roq log [--today|--month|--last N] [--profile NAME] [--outcome TYPE] [--json]
roq skill install [--global|--project|--codex] [--force]
```

Shortcut subcommands only generate SQL — they pass through the exact same
gate + audit + archive pipeline as hand-written queries.

### Accepted statements

Exactly **one** statement per invocation, whose first keyword is one of
`SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES`.
Anything else is rejected before touching the network.

### Output formats (`--format`, default `tsv`)

| Format | Notes |
|---|---|
| `tsv` | Tab-separated; NULL renders as `\N`; `\t` `\n` `\r` inside cells are escaped to visible sequences so rows stay single-line |
| `json` | Single document: `{"columns":[...],"rows":[[...]],"rows_returned":N,"truncated":bool,"elapsedMs":N}`; NULL is `null` |
| `csv` | RFC 4180; NULL is an empty field |

`json` and `csv` are interchange formats — **cells are never truncated** (`--max-cell` only
affects tsv terminal display).

### Limits and truncation

- Default caps: 500 rows (`--max-rows`), 200 characters per tsv cell (`--max-cell`) — a
  flood-guard for the terminal, never for the archive.
- `SHOW CREATE TABLE` is exempt from cell truncation: the DDL is the deliverable and is printed
  in full (an explicit `--max-cell` overrides this).
- When a tsv cell is truncated, stderr says so and points to the archived full copy.
- `--out FILE` writes the result to a file instead (full fidelity, chosen format) and replaces
  the automatic archive; the audit record points at that path.
- `--quiet` suppresses stderr notices (prod reminder, timing, truncation notice); errors and
  exit codes are unaffected, the audit log is always written.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | usage / configuration error |
| `2` | statement rejected by the read-only gate |
| `3` | connection / execution error |
| `4` | `roq diff` found a mismatch |

### Reconciliation with `roq diff`

Runs the same SQL on two profiles and compares ordered column names, row counts, and a
type-aware multiset digest of all rows (order-insensitive, duplicate-sensitive — numeric `1` and
text `'1'` do not compare equal). On mismatch it prints sample rows present on only one side
(`--sample N`, default 5 per side, `0` disables; JSON output exposes them as
`sample.leftOnly` / `sample.rightOnly`). Exactly two `--profile` flags must be given explicitly,
so you never diff against the wrong database. Each side writes its own audit entry.

### Saved queries with `roq q`

Store a recurring SQL statement as a named template, then re-run it with parameters:

```bash
# Save: {{name}} marks a parameter placeholder in the template
roq q add cnt-by-day "SELECT COUNT(*) AS cnt FROM t_example WHERE created >= '{{day}}' AND status = '{{status}}'"

# Run: k=v arguments fill the placeholders by name; query flags work as usual
roq q cnt-by-day day=2026-11-01 status=PAID --profile myproj-prod --format json --quiet

# Manage
roq q list                    # query names + first line of each template
roq q show cnt-by-day         # print the template file verbatim
roq q remove cnt-by-day --yes
```

- Templates live in `~/.roq/queries.d/<name>.sql`, one file per query. The file starts with a
  `--` comment header (documentation); the rest is the SQL template — hand-editable, effective
  immediately. Names are limited to letters, digits, `_`, and `-`, and cannot shadow
  subcommand names (`config`, `log`, `q`, `add`, …). Re-adding an existing name requires
  `--update`.
- Substitution is macro-style text replacement, not bound parameters: missing, extra, or
  duplicate `k=v` arguments are rejected, and values may not contain `'` or `\` — either would
  change the SQL's meaning. To represent such characters, rewrite the template and escape with
  `''` inside it.
- **Saving is not a permanent pass.** At save time the template with dummy values passes a gate
  precheck, so static violations surface early; at run time the substituted SQL goes through the
  full gate again — a gate rule change can reject a query that used to pass.
- Failures before any connection is attempted (unknown query, bad parameters) are audited under
  the `param-error` outcome. Saved-query audit entries carry `saved` / `params` fields, and the
  archive file is named after the query, with the query name and parameters recorded in its
  `# ` header.

## Connection profiles

### Managing profiles with `roq config`

```bash
roq config add <name> --host H --user U --password P --database D [--port 3306] [--ssl] [--file PATH]
roq config add <name> ... --password-env VAR_NAME   # credentials stay off disk
roq config add <name> ... --update [--password P]   # re-record an existing profile
roq config test <name>
roq config remove <name> --yes
roq config list
roq config migrate [--config PATH]                  # legacy plaintext → DPAPI ciphertext, in place
```

Profiles live under `~/.roq/` — either `profiles.conf` (single file) or `profiles.d/*.conf`
(**one file per project**). Both can coexist; duplicate section names across files are an error
(no silent shadowing). `--list` shows each profile's host:port, database, and source file.
`roq config add` writes, then reads back and verifies; existing names are never silently
overwritten.

`--profile` defaults to `dev`. Production profiles should be named `prod` explicitly — the tool
prints a reminder on every query.

### Password encryption (Windows)

On Windows, `--password` is encrypted with DPAPI before it touches disk
(`enc:v1:dpapi:` prefix + base64 ciphertext), bound to the current Windows user: a copied config
file cannot be decrypted on another machine or under another account. The password exists in
plaintext only at entry time. Non-Windows platforms reject `--password` and use
`--password-env` instead (fails fast if the variable is unset — never falls back to plaintext).
`roq config migrate` converts legacy plaintext passwords in place: it decrypts to verify before
writing, touches only `password=` lines, and is idempotent.

**What encryption does not protect against**: code running as your Windows user can decrypt the
ciphertext. Encryption defends against casual reads and file leaks (screenshots, accidental
shares, archive exposure) — the real wall is the database side:

### Use a read-only database account

The tool's guarantees cover statements issued through roq; the account's own privileges are the
last line of defense and the only one client-side hardening cannot replace. Create a dedicated
account for roq and grant it nothing but `SELECT`:

```sql
CREATE USER 'roq_ro'@'%' IDENTIFIED BY 'strong-password';
GRANT SELECT ON target_db.* TO 'roq_ro'@'%';
-- no INSERT / UPDATE / DELETE / DROP — nothing else
```

Even if credentials leak, no write is possible.

### Manual configuration

Writing the INI files by hand is still supported:

```ini
[name]
host=db.example.com
port=3306            ; optional, default 3306
user=roq_ro
password=secret      ; or password_env (the two are mutually exclusive)
password_env=VAR     ; password read from this environment variable; unset ⇒ error before connect
database=mydb
ssl=true             ; optional: TLS encryption (certificate not verified)
```

`--config PATH` accepts a file or a directory (directories are scanned for `*.conf` in name
order). If the server rejects TLS (`Client requires secure connection`), remove `ssl=true` —
that endpoint does not support it.

## Security model: three read-only layers

1. **Local statement gate** — runs before any network I/O.
   - First-keyword whitelist; **quote/comment-aware** multi-statement detection (a `;` inside a
     string literal, e.g. `LIKE '%&nbsp;%'`, is legal).
   - Line comments (`-- `, `#`) are stripped — banned words in comments do not matter; unclosed
     quotes are rejected.
   - Block comments `/* */` are rejected outright (MySQL version comments `/*! */` can execute
     code).
   - `EXPLAIN ANALYZE` is rejected (it really executes the underlying statement).
   - Full forbidden-word scan on `SELECT/WITH/TABLE/VALUES` (`INSERT`, `UPDATE`, `DELETE`, `SET`,
     `INTO`, `LOAD_FILE`, `SLEEP`, …) with word-boundary matching — column names like
     `update_time` are not false positives.
2. **Server-side read-only session** — after connecting, roq issues
   `SET SESSION TRANSACTION READ ONLY` and **asserts the flag by reading it back**
   (`@@session.transaction_read_only` must be `1`; SET failure, read-back failure, or a wrong
   value all abort the query), and sets a 30-second `max_execution_time` fuse.
3. **Output caps** — row and cell limits keep large results from flooding the terminal; cell
   escapes preserve both fidelity and single-line TSV structure.

## Audit log and result archive

Automatic and **cannot be disabled** — this is the tool's ledger of record.

- **Audit log**: `~/.roq/logs/roq-YYYYMM.jsonl`, one JSON line per invocation:
  `ts / profile / db / sql / rows / truncated / ms / outcome / reason / result`. Outcomes include
  `ok`, `param-error` (saved-query substitution failure, with the reason), `gate-rejected`
  (with the rejection reason), `connect-error`, `exec-error`, `config-error` — rejected attempts
  are logged too. Query it with `roq log` (defaults to today; `--month` / `--last N` for wider
  views).
- **Result archive**: `.roq/results/YYYYMMDD/HHMMSS-mmm-<table>-<profile>.tsv` inside the
  current project directory (falls back to `~/.roq/results/` outside a project). A `# ` header
  block records time, database, row cap, and SQL; cells are **never truncated**. The file name
  includes the table — the query name for saved queries, whose headers also record the
  parameters used — so you can tell what was queried without opening it. Add `.roq/` to your
  project's `.gitignore`.

## Roadmap

- [ ] More database drivers: PostgreSQL / SQLite (`driver=` profile field, dialect-aware gate)
- [ ] Config-free one-shot connections: `--dsn "mysql://user:pass@host/db"`

Release history: see [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE)
