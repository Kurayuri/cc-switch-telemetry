# CC Switch Telemetry

CC Switch Telemetry collects usage accounting from multiple cc-switch nodes.
Protocol v3 uses one Client-owned usage ledger as the source of truth, transfers
retained request detail and historical daily rollups atomically, and keeps
cross-`data_source` adjudication out of the Server.

## Current accounting contract

- `--source cc-switch` is the default and exact-data collector. It reconciles
  cc-switch `proxy_request_logs`, `usage_daily_rollups`, and provider catalog
  from `CC_SWITCH_DB` (default `~/.cc-switch/cc-switch.db`) into the shared
  Client ledger without writing to the source database. Stored request costs,
  token semantics, and fractional rollup latency are preserved; they are not
  recalculated by telemetry.
- This repository carries the Tauri-free `cc-switch-usage-core` policy crate
  under `crates/cc-switch-usage-core` for fresh-input semantics, Decimal cost
  calculation, app/model display normalization, cross-source deduplication,
  and rollup range boundaries. A normal clone is therefore self-contained;
  policy changes must still be reviewed against cc-switch accounting behavior.
- `--source local` is an explicit fallback that parses raw Claude, Codex,
  Gemini, OpenCode, Grok Build, and Pi data into the same Client ledger and
  retains every imported request-detail row. Run only one collector mode in a
  Client process. Exact `(app_type, request_id)` conflicts are owned by the
  active collector; collector-scoped cleanup does not erase another
  collector's unrelated rows. Local mode scans those tools' session stores
  directly and prices reconstructed usage from models.dev, so its history and
  costs are not expected to equal cc-switch's persisted accounting.
- `--source local-compact` uses the same raw parsers but aggregates complete
  source-local days older than 30 days into `usage_daily_rollups` and deletes
  only the corresponding `proxy_request_logs`. The two tables together remain
  the complete local history.
- The repository includes a byte-identical snapshot of all six cc-switch parser
  modules at commit `3217f72596f2d1c0f879f0a05f83803825d9809f`; the
  Tauri-free `session-usage-core` adapter owns the executable local-mode path.
  Exact mode remains the parity authority because it also includes cc-switch's
  application-level transactions, cursor recovery, pricing, and source
  adjudication policy.

The Client suppresses proxy/session duplicates before they enter its ledger.
The Server stores exactly what the Client sends and never applies a second
cross-source heuristic. A request row is identified by
`(server-derived node_id, raw app_type, request_id)`; `provider_id` is mutable
metadata and can be corrected without creating another logical request.

The server is the only writer of the central SQLite database. A client token
maps to one server-managed node UUID; node identity is never accepted from an
upload body.

## Workspace layout

- `cc-switch-usage-core`: repository-local, Tauri-free accounting policy shared
  by the exact-data client and server queries.
- `telemetry-core`: protocol-v3 request/response and mutation types.
- `telemetry-client`: source mirror, durable local ledger, hash baseline, and
  uploader, plus the independent Codex quota collector/history ledger.
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

## Run protocol v3

Start the server:

```bash
ADMIN_PASSWORD='set-outside-the-repository' \
TELEMETRY_DB='./data/telemetry.db' \
TELEMETRY_LISTEN='127.0.0.1:8787' \
  cargo run -p telemetry-server
```

Use `http://127.0.0.1:8787/admin` to create a node and obtain its one-time
Bearer token. Start that node's exact-data client:

```bash
CC_SWITCH_DB="$HOME/.cc-switch/cc-switch.db" \
TELEMETRY_LOCAL_USAGE_DB='./data/local-usage.db' \
TELEMETRY_QUOTA_DB='./data/quota-history.db' \
TELEMETRY_QUOTA_INTERVAL_SECONDS='60' \
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
`--source local` to keep all request detail, or `--source local-compact` to
apply the 30-day daily-rollup policy. Changing the parser revision requires
rebuilding the local ledger so historical rows are not mixed across parser
contracts. Returning from compacted history to full detail also requires an
explicit full rebuild so old raw sessions are parsed again:

Do not use either local mode to reconcile the Server against cc-switch. For
that operation, `--source cc-switch` is the only authoritative source.

```bash
cargo run -p telemetry-client -- \
  rebuild --source local --replace-all --upload
```

Before or after an upload, compare the exact source and local mirror read-only:

```bash
cargo run -p telemetry-client -- verify --source cc-switch
```

The verifier compares stable detail/rollup keys and content hashes and reports
only counts plus the first mismatching key; it does not modify either database.

### Codex quota history

Quota collection is deliberately separate from request accounting. On startup
the client immediately enumerates all `app_type='codex'` providers from
`CC_SWITCH_DB`, then queries them sequentially every minute with:

```text
cc-switch-cli --app codex provider quota PROVIDER_ID --json
```

`cc-switch-cli` remains responsible for official subscription, Codex OAuth,
and custom Usage Query credential/account routing. Telemetry checks the JSON
`status`, `available`, and `result` fields even when the command exits zero,
then retains only allowlisted normalized metrics. It never stores or uploads
account IDs, credential messages, raw errors, provider settings, or Usage Query
extras/invalid messages.

Every successful sample is committed to the independent local quota database
before upload. Neither the local database nor the central quota tables have an
automatic pruning or compaction path. Network failures leave the remote cursor
unchanged. To resend the complete local history idempotently to the currently
configured server:

```bash
cargo run -p telemetry-client -- quota replay
```

This database is not touched by `telemetry-client rebuild`. Back it up as an
independent, non-reconstructable source of historical quota samples.

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
| `CC_SWITCH_CLI` | quota client | PATH, then `$HOME/.local/bin/cc-switch-cli` | Explicit executable path override for the supported quota command. |
| `TELEMETRY_QUOTA_DB` | quota client | `./data/quota-history.db` | Independent, durable, non-pruning quota history and per-remote upload cursors. |
| `TELEMETRY_QUOTA_INTERVAL_SECONDS` | quota client | `60` | Sequential quota polling period; `0` disables quota collection. |
| `TELEMETRY_MODELS_DEV_URL` | local client | `https://models.dev/api.json` | Raw-mode pricing endpoint override. |
| `TELEMETRY_CLAUDE_DIR`, `TELEMETRY_CODEX_DIR`, `TELEMETRY_GEMINI_DIR`, `TELEMETRY_OPENCODE_DB`, `TELEMETRY_GROK_DIR` | local client | tool defaults | Claude, Codex, Gemini, OpenCode, and Grok raw-source overrides. |
| `TELEMETRY_PI_SESSION_DIR` | local client | `$HOME/.pi/agent/sessions` | Pi flat or project-directory session root. |

Reqwest also follows standard `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and
`NO_PROXY` variables.

## Dashboard semantics

Open `http://127.0.0.1:8787/dashboard/`. Dashboard assets and APIs are
loopback-only; use SSH port forwarding for remote viewing instead of exposing
them through an unauthenticated proxy.

Trend, quota, and daily calendar visualizations use the self-hosted Apache
ECharts 6.1.0 ESM bundle in `crates/telemetry-server/web/vendor/`; no CDN or
frontend build step is required. The bundle, license, notice, version, and
integrity metadata are packaged with the server source.

Summary, trend, and breakdown queries combine:

1. Client-adjudicated retained detail, with every Server row counted exactly
   once and no additional Server-side proxy/session deduplication.
2. A rollup only when the requested half-open range `[from, to)` fully covers
   that source-local day using its transferred UTC bounds.

The Server builds disposable, dimension-complete hourly aggregates in the
background when its shared writer is idle. Only completed UTC hours marked
`clean` are read from cache. Dirty hours, partial range edges, and trend buckets
that cannot be represented exactly fall back to `usage_events`, so cache work
never changes query results. Client-authored `usage_daily_snapshots` remain
authoritative retained history and are not part of this derived cache.

Fresh input, Claude Desktop folding, and effective pricing-model grouping use
the shared cc-switch policy. Latency is request-count weighted. Request-list
pagination remains retained-detail only because rolled-up rows no longer have
request-level identity.

The Codex quota panel uses independent node, provider, and metric selectors.
Each `(server-derived node UUID, provider ID)` remains a separate series. Long
ranges are automatically limited to roughly 2,000 real points per series by
selecting the final sample in each bucket; values are never averaged and
missing samples remain visible as gaps. Provider status cards remain visible
when quota is unsupported, unconfigured, expired, or temporarily failing.
Explicit utilization and balances derivable from `used / total` or
`(total - remaining) / total` use the fixed 0–100% left axis. Only balances
without a usable total use the right amount axis; their native unit remains in
the legend.

The Server never performs scheduled retention compaction. It retains every
uploaded detail event unless the Client supplies a daily rollup for that exact
complete source-local day. Applying that Client rollup replaces only the
overlapping central detail, so central `usage_events` plus
`usage_daily_snapshots` represents the same complete, non-overlapping history
as Client `proxy_request_logs` plus `usage_daily_rollups`.

Overview responses report `dataScope=detailAndRollup`. `coverage` additionally
reports `includesDetail` and `includesRollups`.

## Protocol v3 API

Authenticated node endpoints:

- `POST /v3/sync/begin`
- `POST /v3/sync/events`
- `POST /v3/sync/rollups`
- `POST /v3/sync/providers`
- `POST /v3/sync/commit`
- `POST /v3/quota/observations`

Loopback-only Dashboard endpoints:

- `GET /v3/dashboard/overview`
- `GET /v3/dashboard/daily`
- `GET /v3/dashboard/filters`
- `GET /v3/dashboard/events`
- `GET /v3/dashboard/quota?from=&to=&bucket=&node_id=&provider_id=`

Authenticated Admin log lifecycle endpoints:

- `GET /admin/api/logs/preview?kind=request|quota&node_id=`
- `POST /admin/api/logs/purge`

The Admin UI requires previewing the exact affected tables and typing the
Server-provided confirmation string before deletion. Request and quota deletion
are separate operations and may target all nodes or one node. Request deletion
also removes derived cache and synchronization staging, but preserves the node,
its token, and provider labels. Deleted request history is not automatically
replayed from an unchanged Client baseline; use an explicit Client rebuild with
`--replace-all --upload` when restoration is intended.

`GET /healthz` is unauthenticated. Product `/v1/*` and protocol `/v2/*` routes
return HTTP 426 after cutover.

## Maintenance-window cutover

Starting the v3 Server migrates an existing v2 database transactionally. It
rebuilds request identity and synchronization tables, preserves request detail,
daily rollups, quota history, nodes, tokens, and provider labels, discards only
incompatible uncommitted generation/staging state, and runs a foreign-key
check. Do not perform the first migration while old clients or the old Server
are still writing.

Recommended sequence:

1. Stop old clients and the old server; retain an immutable backup of the old
   database and its WAL/SHM as a consistent SQLite backup.
2. Start the v3 Server once with `TELEMETRY_DB` pointing at the backed-up
   database, then verify `/healthz`, `PRAGMA integrity_check`, and
   `PRAGMA foreign_key_check`.
3. On every node, install the v3 Client and rebuild its local ledger. The v3
   schema revision intentionally rejects an old source-bound ledger:

   ```bash
   cargo run -p telemetry-client -- \
     rebuild --source cc-switch --replace-all --upload
   ```

4. Compare per-node request counts, complete-day totals, quota history, and
   recent detail against the source before ending the maintenance window.

Rollback is file-level and binary-level: stop v3 components, restore the
consistent v2 database backup and old binaries, then restart the old services.
A v2 Client cannot upload to v3 because all v2 routes intentionally return 426.

## User systemd services

Install automatically creates the deployment launcher and service unit under
`artifacts/`. The launcher is copied from its matching `scripts/run_*.sh`
template only when it does not already exist, so rerunning install never
overwrites local credentials. The generated artifact unit is linked into
`~/.config/systemd/user/`. Artifact files are ignored by Git because launchers
may contain credentials.

On a first install, replace `xxxxx` in the generated launcher and rerun the
installer. A launcher that still contains the placeholder is linked but is not
enabled or started. Install or remove the release-mode user services explicitly:

```bash
./scripts/install-server-service.sh
./scripts/install-client-service.sh

./scripts/uninstall-client-service.sh
./scripts/uninstall-server-service.sh
```

Install scripts build the selected release binary, render the artifact unit,
link it into the user systemd directory, enable it, restart it, and verify that
it is active. Uninstall scripts stop the service and remove the systemd link
plus generated artifact unit; they preserve project data, release binaries,
and credential-bearing artifact launchers.

No command in this repository rotates credentials or switches live database
paths automatically. Service deployment occurs only when an install or
uninstall script above is invoked explicitly.

## Data and security boundaries

- Uploaded records contain usage metadata, not API keys, prompts, response
  bodies, or raw session text.
- Quota uploads contain provider aliases, status classes, normalized numeric
  metrics, sample times, and reset times only. Browser code never calls
  cc-switch or `wham/usage`; it uses the same-origin server Dashboard API.
- Node identity for quota is derived from the existing Bearer token. A quota
  upload body has no node-ID field and cannot select another node namespace.
- Provider labels use `(node_id, app_type, provider_id)` as the stable key; a
  current rename changes display labels without rewriting historical usage.
- Daily rollups use normalized fresh-input semantics version 2 and carry exact
  source-day UTC bounds. The server does not generate rollups; applying a
  client-supplied rollup removes only that node's overlapping central request
  detail to prevent double counting.
- Existing shell scripts in a deployment may contain local credentials. Keep
  credentials outside source files; this implementation neither reads nor
  migrates those script values.

## Dashboard quota settings

The Server creates `settings.json` alongside `TELEMETRY_DB` (normally
`data/settings.json`) on startup. The Admin page provides default provider
selection, independent metric selection per provider, and display aliases scoped
to `(nodeId, providerId)`. Saving applies immediately to the Server's settings;
Dashboard visitors load defaults when opening the page or using **Restore
defaults**. Automatic refresh preserves their current selection and updates
aliases.

`GET /admin/api/settings` returns `{ settings, providers }`, including the known
metric catalog. `PUT /admin/api/settings` accepts and returns the settings object;
both require the existing Admin session. `GET /v3/dashboard/settings` exposes the
same display settings under the Dashboard's loopback access policy.

```json
{
  "version": 1,
  "quotaDefaults": {
    "providers": [
      {
        "nodeId": "node-uuid",
        "providerId": "provider-id",
        "metrics": [{ "key": "weekly", "kind": "utilizationPercent", "unit": "%" }]
      }
    ]
  },
  "quotaProviderAliases": [
    { "nodeId": "node-uuid", "providerId": "provider-id", "alias": "My subscription" }
  ]
}
```

`providers: null` selects every provider and metric. Within a selected provider,
`metrics: null` selects all its metrics. Empty arrays select nothing. An empty
alias restores the collected provider name. Unavailable selections are retained.
Updates use an atomic file replacement; invalid existing files cause a startup
error instead of being silently replaced. Direct file edits require a restart.

Quota history segments raw observations before downsampling: intervals of up to
600 seconds connect, while longer gaps or changes between percentage and amount
axes start a new segment. Each bucket retains its last real value and every
segment retains its endpoints. Usage and daily tooltips display `Totel` using
`realTotalTokens` (input including cache tokens, plus output).
