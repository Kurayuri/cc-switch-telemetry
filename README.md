# CC Switch Telemetry

CC Switch Telemetry is an independent Rust workspace for collecting usage
statistics from distributed cc-switch installations. The client reads the
usage data that cc-switch has already materialized in SQLite and uploads it to
the server. It does not scan Claude, Codex, Gemini, or other raw session-log
files.

The server is the sole writer of the central SQLite database. It provides
authenticated ingestion, idempotent event handling, summary APIs, and an
embedded local dashboard.

## Workspace layout

- `telemetry-core`: shared wire-format types and identifiers.
- `telemetry-client`: read-only cc-switch database reader and uploader.
- `telemetry-server`: central SQLite collector, HTTP API, and embedded dashboard.

## Quick start

Build the workspace with the installed Rust toolchain:

```bash
cargo build --workspace
```

Start the server:

```bash
TELEMETRY_TOKEN='replace-with-a-long-random-token' \
  cargo run -p telemetry-server
```

Start one client node. Use the real path to the local cc-switch database:

```bash
CC_SWITCH_DB="$HOME/.cc-switch/cc-switch.db" \
TELEMETRY_SERVER_URL='http://127.0.0.1:8787' \
TELEMETRY_NODE_ID='node-a' \
TELEMETRY_TOKEN='replace-with-a-long-random-token' \
  cargo run -p telemetry-client
```

The client should use a stable, unique `TELEMETRY_NODE_ID` for each cc-switch
installation. If `TELEMETRY_TOKEN` is set on the server, the same value must be
set on every client.

## Environment variables

The variables below are read directly by the binaries. A variable marked
`Server and Client` has the same meaning on both sides and should be configured
consistently.

| Variable | Used by | Default | Meaning |
| --- | --- | --- | --- |
| `TELEMETRY_TOKEN` | Server and Client | Unset | Shared bearer token. When set on the server, protected ingestion and summary endpoints require `Authorization: Bearer <token>`; the client sends this token with uploads. When unset, those endpoints do not require authentication. |
| `TELEMETRY_DB` | Server | `./data/telemetry.db` | Path of the server's central SQLite database. The server creates the parent directory and initializes or migrates its schema on startup. |
| `TELEMETRY_LISTEN` | Server | `127.0.0.1:8787` | Local address and TCP port on which the server listens. Use an externally reachable address, such as `0.0.0.0:8787`, only when remote clients need to upload directly. |
| `CC_SWITCH_DB` | Client | `~/.cc-switch/cc-switch.db` | Path of the local cc-switch SQLite database. The client opens it read-only and reads `proxy_request_logs` plus the `providers` table. The binary does not expand `~`, so use an absolute path or expand it in the shell when setting this variable. |
| `TELEMETRY_SERVER_URL` | Client | `http://127.0.0.1:8787` | Base URL of the telemetry server. The client appends `/v1/events/batch` and `/v1/providers/snapshot` when uploading data. |
| `TELEMETRY_NODE_ID` | Client | `node-1` | Stable identity of this client node. It is included in every event and provider snapshot and participates in event idempotency. Use a different value for each source cc-switch installation. |
| `TELEMETRY_STATE` | Client | `./data/client-cursor.json` | Persistent client cursor file. The client atomically stores the last acknowledged `(created_at, request_id)` position here and resumes from it after restart. |

The client also follows reqwest's standard proxy discovery. These are not
`cc-switch-telemetry`-specific variables:

- `HTTP_PROXY` / `http_proxy`: proxy for HTTP requests.
- `HTTPS_PROXY` / `https_proxy`: proxy for HTTPS requests.
- `ALL_PROXY` / `all_proxy`: fallback proxy for supported schemes.
- `NO_PROXY` / `no_proxy`: comma-separated hosts or addresses that should bypass
  the proxy.

The client does not hard-code a loopback bypass; proxy behavior is determined by
reqwest and these environment variables.

## Client synchronization

The client opens the cc-switch database in read-only mode and watches both the
main database file and its `-wal` file. It polls their modification time and
size every five seconds. A detected change triggers synchronization immediately;
when there is no change, the client does not query SQLite.

Each scan:

- Reads up to 512 rows per batch from `proxy_request_logs` using the composite
  cursor `(created_at, request_id)`, so multiple requests in the same second are
  not skipped.
- Re-scans the previous 10 minutes to tolerate delayed session-log imports and
  relies on server-side idempotency to suppress duplicates.
- Drains full batches continuously without sleeping between batches.
- Sends a provider-name snapshot from cc-switch's `providers` table.
- Retries transient connection failures and HTTP `408`, `425`, `429`, `500`,
  `502`, `503`, and `504` responses with exponential backoff.
- Advances the cursor only after the server acknowledges the batch. Logs report
  `sent`, `accepted`, `duplicates`, and `rejected` counts.

The server derives the event id as `node_id + ":" + request_id` and stores it
with a uniqueness constraint. Re-uploading an already accepted event therefore
does not count or charge it twice.

## Dashboard

Open the dashboard locally at:

```text
http://127.0.0.1:8787/dashboard/
```

The HTML, CSS, and JavaScript are embedded in the server binary. No Node.js,
CDN, or separate frontend deployment is required.

The dashboard provides:

- Request count, token usage, estimated cost, success rate, cache hit rate, and
  average latency.
- Fixed or custom trend buckets, including local-calendar-day views and zero-
  filled buckets with no events.
- Top-10 breakdowns by node, application, provider, and model.
- Combined time, node, application, provider, model, and data-source filters.
- Stable cursor pagination for recent requests.
- English and Simplified Chinese localization, with browser-language detection
  and persisted user selection.
- Dark and light themes with persisted user selection.
- Automatic refresh every 30 seconds; refresh pauses while the browser tab is
  hidden.

Dashboard HTML, assets, and `/v1/dashboard/*` APIs are restricted to loopback
clients. This restriction is independent of `TELEMETRY_TOKEN`: remote clients
can upload data to the server's listening address, but cannot access the
dashboard through that address.

To view the dashboard from another machine, use SSH port forwarding:

```bash
ssh -L 8787:127.0.0.1:8787 user@server-host
```

Then open `http://127.0.0.1:8787/dashboard/` locally. Do not expose the
dashboard through an unauthenticated reverse proxy. The server intentionally
ignores `X-Forwarded-For`; a reverse proxy on the same host is still seen as a
loopback peer.

## Data boundaries and semantics

- The client reuses cc-switch's materialized `proxy_request_logs` records. It
  does not read raw provider files or session-log text.
- `created_at` is a Unix epoch timestamp in seconds.
- Detail events are idempotent by node and request. Daily rollup snapshots are
  a separate ingestion path reserved for historical days whose detail rows have
  been removed by cc-switch retention.
- The dashboard is intentionally **detail-only**: it queries `usage_events` and
  does not combine them with `usage_daily_snapshots`. Older data may therefore
  have incomplete coverage; the dashboard reports the covered event range.
- Token normalization and cache-hit calculations follow cc-switch semantics.
  Successful requests are those with HTTP status codes from `200` through
  `299`.
- Displayed cost is a local estimate calculated from uploaded event pricing
  data, not a provider invoice.
- Provider names are synchronized per node and application type. Historical
  dashboard rows use the current mapped name when available and fall back to the
  provider ID when no mapping exists.
- Uploaded data does not contain API keys, prompts, response bodies, or raw
  session text.

## HTTP API

### Health endpoint

- `GET /healthz` is unauthenticated and reports server/database health.

### Authenticated endpoints

These endpoints use `TELEMETRY_TOKEN` when it is configured:

- `POST /v1/events/batch`
- `POST /v1/rollups/snapshot`
- `POST /v1/providers/snapshot`
- `GET /v1/usage/summary`

### Loopback-only dashboard endpoints

- `GET /dashboard/`
- `GET /dashboard/app.js`
- `GET /dashboard/i18n.js`
- `GET /dashboard/range.js`
- `GET /dashboard/styles.css`
- `GET /v1/dashboard/overview`
- `GET /v1/dashboard/daily`
- `GET /v1/dashboard/filters`
- `GET /v1/dashboard/events`

Dashboard query parameters include:

- `from` and `to`: Unix seconds defining the half-open range `[from, to)`;
  default is the most recent 24 hours and the maximum range is 365 days.
- `node_id`, `app_type`, `provider_id`, `model`, and `data_source`: optional
  exact-match filters.
- `bucket`: for overview trends, `auto`, a fixed bucket such as `5m` or `1d`,
  or a custom integer duration such as `10m`. `auto` selects a suitable bucket
  and returns the resolved value in `range.bucket`.
- `tz_offset_minutes`: timezone offset used for local-day alignment.
- `limit`: for events, defaults to 50 and is capped at 200.
- `before_created_at` and `before_event_id`: paired cursor parameters for
  loading older events.

The server is the only writer of the central SQLite database. Do not let
multiple nodes open a shared network SQLite file directly.

## Development checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --check crates/telemetry-server/web/app.js
```
