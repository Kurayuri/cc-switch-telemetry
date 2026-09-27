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
  modules at commit `87d966b7f887adfe0e9856ee0f7e93cc8efc874f`; the
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

## Fast accounting compatibility

The importer and fixed Fast tariff policy are pinned to cc-switch
`build/codex-fast-fix` at `87d966b7` (schema 21). Exact `cc-switch` mode preserves
stored costs without applying Fast factors again. Local modes use the pinned
model/tier rules, including dated GPT snapshots, with no guessed aliases or
context-length surcharge. Historical metadata enrichment uses retained component
prices; independently reported totals and already compacted days are preserved.
The Codex adapter merges paginated parents, checks finalized fork boundaries and
retries deferred children without advancing their durable cursors. Cursor/data
writes commit together. The known previous importer revision upgrades additively;
a one-time reconciliation removes obsolete local Codex replay rows only after
successful parsing. Other collector rows, daily rollups and upload baselines remain.

Protocol v3 adds optional `serviceTier`, `serviceTierSource`, `reasoningEffort`,
and `serviceTierPricingVersion` event fields, plus optional `diagnosticCode` on
quota provider states. **Upgrade the Server before Clients**: the new Server
accepts missing fields from old Clients, while old Servers reject new fields.
Event hashes include present metadata so amendments to old timestamps propagate.
Missing values remain unknown. Request/response tier sources remain distinct;
a requested tier is not confirmation of provider billing.

`GET /v3/dashboard/events` accepts `service_tier` and `reasoning_effort`.
These filters affect request detail only, including pagination; KPI, trend,
daily/breakdown totals and historical quota references keep their existing scope.
The `unknown` choice matches absent metadata. `fast` matches Fast and non-Claude
Priority, following the pinned cc-switch request-list rule. Other tier/effort
choices are exact matches. Compressed historical days have no request detail to
filter and are not reconstructed.

Quota collection now uses the cc-switch fix4 local API. Historical
`cli_schema_incompatible` states remain readable; current API failures use the
fixed `quota_api_unavailable` diagnostic. No credentials or raw errors are uploaded.

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

The client polls the **cc-switch fix4 local quota API** every 60 seconds. Keep
cc-switch running on the same node. It discovers the authenticated loopback
endpoint through `quota-api.json` beside `CC_SWITCH_DB`; override the path with
`CC_SWITCH_QUOTA_API_FILE` on both applications when needed. The file is mode
0600 and contains a short-lived local capability: never upload or share it.

Provider discovery and quota reads use `/v1/quota/providers` and
`/v1/quota/query`. cc-switch owns provider-to-account mapping, managed OAuth
refresh, native subscription and saved usage-script queries, and outbound proxy
configuration. Telemetry receives only quota fields and public status codes.
It does not invoke `cc-switch-cli`, read source quota credentials, or require a
particular source database schema for quota collection. There is no CLI fallback.

Closing cc-switch makes the quota API unavailable; the client records failure
states for known providers without fabricating fresh samples from old values.
The next polling cycle re-reads discovery, so cc-switch restarts and token/port
rotation recover automatically. Upgrade the telemetry server first to accept
the `quota_api_unavailable` diagnostic, then upgrade clients and cc-switch.

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
| `CC_SWITCH_QUOTA_API_FILE` | cc-switch / quota client | `quota-api.json` in the cc-switch config directory / beside `CC_SWITCH_DB` | Local API discovery file; no CLI dependency. |
| `TELEMETRY_QUOTA_DB` | quota client | `./data/quota-history.db` | Independent, durable, non-pruning quota history and per-remote upload cursors. |
| `TELEMETRY_QUOTA_INTERVAL_SECONDS` | quota client | `60` | Sequential quota polling period; `0` disables quota collection. |
| `TELEMETRY_MODELS_DEV_URL` | local client | `https://models.dev/api.json` | Raw-mode pricing endpoint override. |
| `TELEMETRY_CLAUDE_DIR`, `TELEMETRY_CODEX_DIR`, `TELEMETRY_GEMINI_DIR`, `TELEMETRY_OPENCODE_DB`, `TELEMETRY_GROK_DIR` | local client | tool defaults | Claude, Codex, Gemini, OpenCode, and Grok raw-source overrides. |
| `TELEMETRY_PI_SESSION_DIR` | local client | `$HOME/.pi/agent/sessions` | Pi flat or project-directory session root. |

Remote HTTP requests follow standard `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`,
and `NO_PROXY` variables. Local quota API calls bypass proxies and redirects.

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
- `GET /v3/dashboard/time-bounds` — earliest stored usage detail, daily rollup, or quota observation.
- `GET /v3/dashboard/quota/resets` — chronological reset runs including zero usage, grouped by node/provider/tier; independent of chart range and buckets.
- `POST /v3/dashboard/quota/cycle-summaries` — read-only raw-sample summaries for 1–128 ended cycles. Body: `nodeId`, `providerId`, `metricKey`, `metricKind`, nullable `unit`, and `cycles: [{from,to,resetsAt}]`. Response: `cycles` with those boundaries plus `reachedFull`, nullable `sampledAt`, and nullable `utilizationPercent`; cycle intervals are `[from,to)` with a 60-second Reset-anchor tolerance.

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
selection, independent metric selection per provider, display aliases scoped to
`(nodeId, providerId)`, Dashboard default range settings, and per-model billing
display multipliers. Saving applies immediately to the Server's
settings; Dashboard visitors load the configured range, custom-input time
format, and model multipliers when opening the page. Automatic refresh
preserves their current range and updates aliases. **Restore defaults**
continues to restore the saved Quota provider and metric selection.

The default range can be a fixed preset or **Last reset**. Last reset stores a
node, provider, and reset-tier metric identity; if it is unavailable when the
page opens, the Dashboard uses the first available reset tier. Custom ranges
are not persisted as defaults. The Usage Trend cumulative checkbox is a
browser-session option and starts unchecked. The time format setting affects
only the two custom range time inputs: `HH:mm` in 24-hour mode or
`hh:mm AM/PM` in 12-hour mode. Each model multiplier defaults to `1` when it
is not configured, accepts `0` to `1000`, and changes only the Dashboard's
read-only cost presentation (summary, trend, daily view, breakdowns, and
events); it does not rewrite SQLite data or Token counts.

Each model selects exactly one billing mode: `overall` (legacy default) or
`components` (separate Fresh, Creation, Read, and Output multipliers).
Inactive mode values never stack. Legacy `input` settings migrate to components
with the same factor for Fresh, Creation, and Read, and Output at 1×. Admin fetches a reference-price snapshot from the same
models.dev catalog used by the collector (`TELEMETRY_MODELS_DEV_URL` can override
the catalog). The authenticated `GET /admin/api/model-pricing?model=...` endpoint
returns `model`, `resolvedModel`, `source`, `fetchedAt`, and USD-per-million
`fresh`, `creation`, `read`, `output` prices; missing prices remain null.
`refresh=true` bypasses the 24-hour catalog cache. Snapshots are saved with the
model's settings and only change when explicitly updated; Dashboard queries do
not fetch live prices. Saving an active non-overall entry without a snapshot attempts server-side
resolution. If unavailable or ambiguous, it retains a null snapshot and original
costs, with an Admin save notice and Dashboard fallback counts; prices are never guessed.

For each record, `weight = normalized category tokens × reference unit price`;
`display cost = original cost × sum(weight × selected factor) / sum(weight)`.
All-one factors preserve the original cost exactly. Missing required prices or
zero/invalid weights retain the original amount and increment
`unadjustedCostRequests` in summaries, buckets, breakdowns and events; the UI
shows the fallback. Adjustments happen before aggregation, bypassing hourly cost
caches for active component adjustments. Legacy daily rollups use their own
aggregate token composition, so their component amounts are estimates.

For example, a model entry can use
`{"model":"gpt-5","mode":"overall","multiplier":2}` or
`{"model":"gpt-5","mode":"components","freshMultiplier":2,"creationMultiplier":1,"readMultiplier":0.5,"outputMultiplier":2}`.
The save response includes the resolved `referencePricing` snapshot and default
fields. Old `{model,multiplier}` entries continue to mean overall mode.

Usage Trend offers a session-only **Compare Quota** switch. Enabling it defaults
to Estimated cost using the existing Usage filters and checks the cumulative
and Predict checkboxes on each enable; both remain freely toggleable. Both charts share the
same manual granularity. In Compare, Usage Auto uses Quota history's bucket rule
(`bucket=quota-auto`); outside Compare, Usage Auto selects the coarsest preset yielding at least 20 buckets (or 1-second buckets for shorter ranges). Switching Compare with Auto reloads only the Usage overview.
The Usage axis starts at zero and includes accumulated values in cumulative mode
and the Estimated quota curve when sharing the cost axis; empty or all-zero data
uses an upper bound of 1.
The selected Quota Provider/window defaults to Last reset and overlays the same
raw percentage history shown in Codex quota history, including reset drops and
data gaps. With Predict off, the overlay reuses loaded Quota data without a separate comparison fetch. Disabling restores the previous
Usage metric. The comparison percentage axis includes the visible actual and predicted
quota maximum (with a 1% upper bound for empty/all-zero data). After those base
ranges are calculated, the latest Usage bucket containing an actual utilization
sample anchors both axes at the same vertical position. Alignment only expands an
axis, so no visible series is clipped; the utilization display axis may exceed
100% even though utilization data remains bounded to 0–100%. Zero or missing
anchors retain the independent base ranges. The two series retain their own timestamps;
hover shows the timestamp of each displayed value (Quota to minute precision).
**History** is enabled by default with the previous full-quota cycle when Compare
Quota is first enabled. Its reference button opens a dialog with None, manual USD
amount, previous full-quota cycle, previous cycle, or a selected ended Reset cycle.
Choosing None and applying disables the reference; the choice is retained when
Compare is toggled during the page session. There is no separate History checkbox. Full means a
cycle that reached 100%, including peaks omitted by chart downsampling. Automatic
sources follow the cycle containing the viewed range's exclusive end (capped at
now); manual amounts and explicitly selected cycles stay fixed. Historical quota
is the selected cycle's cumulative cost through its last valid utilization sample,
divided by that percentage. Usage filters and billing multipliers still apply.
Applying a reference draws a horizontal line and maps its USD amount to 100%,
replacing latest-bucket alignment until disabled. Token and Requests retain their
own axes and use a separate USD axis. All axes retain shared tick heights and
100% stays visible. Predict and Estimated quota remain independently controlled.
An unavailable reference is shown in the control; it never silently reuses a
reference from another identity or falls back to latest-bucket alignment. Cancel
leaves the applied reference unchanged. Data is cached for five minutes; manual
Refresh revalidates it, and stale requests cannot overwrite a newer selection.

Usage point markers are shown for up to 120 points and hidden above that. Compare also supports Predict,
sharing the selected metric's switch and prediction cache with Quota history;
the dotted forecast extends the time axis and is included in quota-axis scaling.
Predict also draws **Estimated quota** from each confirmed reset cycle's cumulative
Estimated cost divided by utilization (as a fraction). It retains the Usage
filters and billing multipliers, queries missing pre-range costs, and starts a
new cost accumulator at each reset. Each point uses a cost bucket's end and the
latest quota sample within that bucket and cycle; the tooltip shows both times.
Zero/missing utilization, unconfirmed cycles, and sampling gaps are not bridged.
This historical estimate does not extend into the future and is independent of
the cumulative display switch or availability of a utilization speed forecast.
It shares the cost axis for Estimated cost and uses a separate USD axis for Token
and Requests. Predict's extra requests use at most three concurrent cycle workers;
selection/filter/range changes cancel stale work. Refreshes retain the previous
complete curves while loading and silently update them without entry animations.
Cycle/prefix caches reuse unchanged data for five minutes; manual Refresh
revalidates them and retries failures without removing the plotted line. Legend
visibility is preserved. Late data and price/multiplier changes invalidate the
affected calculations.
Quota Provider, Quota window, and Predict controls are right-aligned and wrap on
narrow screens. The comparison chart reserves extra header space so its plot stays full height.

`GET /admin/api/settings` returns `{ settings, providers }`, including the known
metric catalog. `PUT /admin/api/settings` accepts and returns the settings object;
both require the existing Admin session. `GET /v3/dashboard/settings` exposes the
same display settings under the Dashboard's loopback access policy.

```json
{
  "version": 1,
  "dashboardDefaults": {
    "rangePreset": "24h",
    "timeFormat": "24h",
    "modelBillingMultipliers": [
      { "model": "gpt-5", "multiplier": 1.25 },
      { "model": "claude-sonnet", "multiplier": 0.8 }
    ],
    "lastReset": null
  },
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

### Past Resets

In **Custom time range → Past Resets**, choose a quota provider, reset tier, and
recorded cycle. Reset detection uses timestamps, without requiring usage to
return to zero. Consecutive observations within 60 seconds of a fixed reset
anchor form a run; three distinct sample timestamps spanning more than 60 seconds
confirm a new reset. Old
reset values returning later do not reopen old cycles. A new cycle's inferred
start (`resetsAt - tier period`) closes the previous cycle early when needed.
Only after detecting boundaries are cycles without positive usage excluded.
Periods use the same tier-name/history inference as Last reset; missing cycles
are not synthesized. Historical sampling may cover only part of a cycle.

Applying a cycle changes only the dashboard time range and preserves usage and
quota filters. Refreshing a current cycle updates its end if a new early reset
is confirmed, without switching the selected cycle. Cancel leaves the applied
range unchanged. This selection is page-local and is not an Admin default.

**All time** starts at the earliest stored usage detail, nonempty daily rollup,
or quota observation across all nodes/providers and ends now. Existing filters
remain selected. The `all_time=true` Dashboard query flag resolves the start on
the server and allows histories longer than 720 days while retaining the trend
point-count limit. Other custom ranges keep their existing limit. All time is
also available as an Admin default. Empty databases show an empty recent window
rather than a range starting in 1970.

### Dashboard read projections and refresh performance

Dashboard filters refresh Usage statistics, events and the daily heatmap without
reloading unchanged Quota data. Time-range changes reuse the full-year heatmap
when its filters and calendar bounds are unchanged. Ordinary trends render as
soon as their statistics arrive; Compare waits only for matching Usage and Quota
ranges. Requests are cancelled per consumer, duplicate in-flight GETs are shared,
and chart updates are coalesced into animation frames. Manual refresh and the
30-second automatic refresh still revalidate the full view.

SQLite retains all original observations and billing amounts. Rebuildable read
projections add indexed Quota samples, latest metrics, consecutive reset runs,
and signed per-event component billing results. Hourly billing sums are computed
from individual adjusted events, not from aggregate token ratios. Pricing changes
invalidate signatures; unready projections fall back to the original calculation.
Quota insert/delete triggers maintain read projections in the same transaction;
late samples invalidate only their metric's reset history. Background work runs
on blocking threads, one reset series, up to 2,000 billing events and one hour per
iteration. Each statistics response reads a consistent SQLite snapshot.

The first startup performs a restartable quota-history backfill in 2,000-row
transactions before opening the listener. On the September 26 validation backup
(about 339,000 quota metrics), initial startup took approximately 25 seconds.
Billing and reset caches warm in the background; subsequent starts reuse them.
Take a SQLite backup (including WAL content through the backup API), settings and
binary backup before deployment. All projections are additive; an older binary
can read the original records, but after running an older writer the projections
must be rebuilt before returning to the optimized version.

For an offline projection rebuild, with every writer stopped, drop the
`quota_metric_project`, `quota_metric_unproject`, `quota_metric_reproject`,
`usage_billing_invalidate`, `usage_billing_delete` and `billing_revision_*`
triggers, then the `quota_sample_cache`, `quota_current_cache`,
`quota_reset_cache`, `quota_projection_meta`, `usage_billing_cache` and
`billing_projection_meta` tables; mark every `usage_cache_partitions` row dirty.
Restart the optimized server to rebuild from original records. Do not delete
`quota_observations`, `quota_metrics` or `usage_events`.

Dashboard assets and JSON support negotiated gzip. Reset history supports ETag
revalidation and optional `node_id`, `provider_id`, `metric_key`, `metric_kind`
and `unit` filters; omitting them preserves the full-history response. Expensive
Dashboard responses include `Server-Timing` for SQL plus projection and JSON
serialization. This excludes network transfer and browser drawing.

Validation helpers:

- `python3 scripts/test-ui-performance-browser.py`: isolated refresh/race regression.
- `python3 scripts/benchmark-dashboard.py <backup-directory>`: compare
  `baseline-server`, `baseline.db` and `optimized.db` with the release binary;
  the directory must also contain the same `settings.json` for both copies.
  Use disposable SQLite backups, never production database paths.
