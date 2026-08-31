# CC Switch Telemetry

CC Switch Telemetry collects usage accounting from multiple cc-switch nodes.
Protocol v2 treats cc-switch as the exact-data source of truth, transfers both
retained request detail and historical daily rollups, and makes each node's
generation visible atomically.

## Current accounting contract

- `--source cc-switch` is the default and exact-data mode. It mirrors the
  materialized `proxy_request_logs`, `usage_daily_rollups`, and provider catalog
  from cc-switch without writing to the source database.
- This repository carries the Tauri-free `cc-switch-usage-core` policy crate
  under `crates/cc-switch-usage-core` for fresh-input semantics, Decimal cost
  calculation, app/model display normalization, cross-source deduplication,
  and rollup range boundaries. A normal clone is therefore self-contained;
  policy changes must still be reviewed against cc-switch accounting behavior.
- `--source local` is an explicit fallback that parses raw Claude, Codex,
  Gemini, OpenCode, Grok Build, and Pi data into an independent ledger. The
  repository includes a byte-identical snapshot of all six cc-switch parser
  modules at commit `3217f72596f2d1c0f879f0a05f83803825d9809f`; the
  Tauri-free `session-usage-core` adapter owns the executable local-mode path.
  Exact mode remains the parity authority because it also includes cc-switch's
  application-level transactions, cursor recovery, pricing, and dedup policy.

The server is the only writer of the central SQLite database. A client token
maps to one server-managed node UUID; node identity is never accepted from an
upload body.

## Workspace layout

- `cc-switch-usage-core`: repository-local, Tauri-free accounting policy shared
  by the exact-data client and server queries.
- `telemetry-core`: protocol-v2 request/response and mutation types.
- `telemetry-client`: source mirror, durable local ledger, hash baseline, and
  uploader.
- `telemetry-server`: staged generation commit, central SQLite store, node
  administration, and embedded Dashboard.
- `session-usage-core`: six-source raw-session adapter and parser-provenance
  tests used by explicit local mode.
- `3rdparty/cc-switch`: exactly six reviewed cc-switch parser modules plus the
  upstream MIT license; provenance and SHA-256 values are recorded in
  `PARSER_SNAPSHOT.md`.

## Build and test

```bash
cargo build --workspace
cargo test --offline --workspace
cargo fmt --all -- --check
cargo clippy --offline --workspace --all-targets -- -D warnings
scripts/sync-session-usage.sh
```

The workspace has no sibling-repository path dependency. The parser snapshot
pin describes the six vendored parser modules; it does not claim that the
repository-local policy crate was tracked by that cc-switch commit.

## Run protocol v2

Start the server:

```bash
ADMIN_PASSWORD='set-outside-the-repository' \
TELEMETRY_DB='./data/telemetry-v2.db' \
TELEMETRY_LISTEN='127.0.0.1:8787' \
  cargo run -p telemetry-server
```

Use `http://127.0.0.1:8787/admin` to create a node and obtain its one-time
Bearer token. Start that node's exact-data client:

```bash
CC_SWITCH_DB="$HOME/.cc-switch/cc-switch.db" \
TELEMETRY_LOCAL_USAGE_DB='./data/local-usage.db' \
TELEMETRY_SERVER_URL='http://127.0.0.1:8787' \
TELEMETRY_TOKEN='node-token-from-admin' \
  cargo run -p telemetry-client -- run --source cc-switch
```

The client watches the source database and WAL. It first reconciles the source
into its local ledger, then opens a server generation:

- A new remote binding, or an explicit rebuild, uploads a full `replaceAll`
  snapshot.
- Normal operation compares content hashes with the committed local baseline
  and sends only event/rollup upserts and deletes.
- Providers are sent as a complete catalog for each generation.
- Staged data is invisible until commit; the local hash baseline advances only
  after a successful server commit. Retrying after a crash is idempotent.

To rebuild the client ledger and force a complete node replacement:

```bash
cargo run -p telemetry-client -- \
  rebuild --source cc-switch --replace-all --upload
```

Explicit six-source raw-session mode uses the same command shape with
`--source local`. Changing the parser revision requires rebuilding the local
ledger so historical rows are not mixed across parser contracts.

Before or after an upload, compare the exact source and local mirror read-only:

```bash
cargo run -p telemetry-client -- verify --source cc-switch
```

The verifier compares stable detail/rollup keys and content hashes and reports
only counts plus the first mismatching key; it does not modify either database.

## Environment variables

| Variable | Component | Default | Meaning |
| --- | --- | --- | --- |
| `ADMIN_PASSWORD` | server | unset | Enables local administrator login; never stored in SQLite. |
| `TELEMETRY_DB` | server | `./data/telemetry.db` | Central SQLite path. |
| `TELEMETRY_LISTEN` | server | `127.0.0.1:8787` | Listener socket address. |
| `TELEMETRY_TOKEN` | client | required | Node Bearer token from `/admin`. |
| `TELEMETRY_SERVER_URL` | client | `http://127.0.0.1:8787` | Server base URL. |
| `CC_SWITCH_DB` | exact client | `$HOME/.cc-switch/cc-switch.db` | Read-only source database path. |
| `TELEMETRY_LOCAL_USAGE_DB` | client | `./data/local-usage.db` | Durable mirror/import ledger and upload-hash baseline. |
| `TELEMETRY_MODELS_DEV_URL` | local client | `https://models.dev/api.json` | Raw-mode pricing endpoint override. |
| `TELEMETRY_CLAUDE_DIR`, `TELEMETRY_CODEX_DIR`, `TELEMETRY_GEMINI_DIR`, `TELEMETRY_OPENCODE_DB`, `TELEMETRY_GROK_DIR` | local client | tool defaults | Claude, Codex, Gemini, OpenCode, and Grok raw-source overrides. |
| `TELEMETRY_PI_SESSION_DIR` | local client | `$HOME/.pi/agent/sessions` | Pi flat or project-directory session root. |

Reqwest also follows standard `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and
`NO_PROXY` variables.

## Dashboard semantics

Open `http://127.0.0.1:8787/dashboard/`. Dashboard assets and APIs are
loopback-only; use SSH port forwarding for remote viewing instead of exposing
them through an unauthenticated proxy.

Summary, trend, and breakdown queries combine:

1. Effective retained detail after node-scoped proxy/session deduplication.
2. A rollup only when the requested half-open range `[from, to)` fully covers
   that source-local day using its transferred UTC bounds.

Fresh input, Claude Desktop folding, and effective pricing-model grouping use
the shared cc-switch policy. Latency is request-count weighted. Request-list
pagination remains retained-detail only because rolled-up rows no longer have
request-level identity.

Overview responses report `dataScope=detailAndRollup`. `coverage` additionally
reports `includesDetail`, `includesRollups`, and the latest committed
`sourceKinds` represented by the selected node set.

## Protocol v2 API

Authenticated node endpoints:

- `POST /v2/sync/begin`
- `POST /v2/sync/events`
- `POST /v2/sync/rollups`
- `POST /v2/sync/providers`
- `POST /v2/sync/commit`

Loopback-only Dashboard endpoints:

- `GET /v2/dashboard/overview`
- `GET /v2/dashboard/daily`
- `GET /v2/dashboard/filters`
- `GET /v2/dashboard/events`

`GET /healthz` is unauthenticated. Product `/v1/*` ingestion, summary, and
Dashboard API routes return HTTP 426 after cutover.

## Maintenance-window cutover

Do not rebuild a live central database in place. The command below opens the
old database read-only, refuses an existing target, creates a fresh v2 schema,
and copies only node UUIDs/token hashes and current provider labels. Usage rows
must be repopulated by v2 clients.

```bash
cargo run -p telemetry-server -- \
  rebuild-v2 --from ./data/telemetry.db --to ./data/telemetry-v2.db
```

Recommended sequence:

1. Stop old clients and the old server; retain an immutable backup of the old
   database and its WAL/SHM as a consistent SQLite backup.
2. Run `rebuild-v2` to a new path and run `PRAGMA integrity_check` (the command
   also checks it before success).
3. Start the v2 server with `TELEMETRY_DB` pointing at the new path.
4. On every node, use the existing token and run the client rebuild command
   with `--replace-all --upload`.
5. Compare node counts, source kinds, complete-day totals, and recent detail
   against cc-switch before ending the maintenance window.

Rollback is file-level and binary-level: stop v2 components, restore the old
server/client binaries, and point `TELEMETRY_DB` back to the untouched old
database. A v2 client cannot fall back to v1 because v1 routes intentionally
return 426.

No command in this repository rotates credentials, edits service definitions,
switches live database paths, or deploys processes automatically.

## Data and security boundaries

- Uploaded records contain usage metadata, not API keys, prompts, response
  bodies, or raw session text.
- Provider labels use `(node_id, app_type, provider_id)` as the stable key; a
  current rename changes display labels without rewriting historical usage.
- Daily rollups use normalized fresh-input semantics version 2 and carry exact
  source-day UTC bounds. Applying a rollup removes that node's overlapping
  central request detail to prevent double counting.
- Existing shell scripts in a deployment may contain local credentials. Keep
  credentials outside source files; this implementation neither reads nor
  migrates those script values.
