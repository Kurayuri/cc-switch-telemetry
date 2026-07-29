# cc-switch-telemetry

独立的 cc-switch usage telemetry client/server。它不读取 Claude/Codex/Gemini 原始日志，而是复用 cc-switch 已物化的 SQLite usage 表。

## 运行

```bash
cargo run -p telemetry-server
CC_SWITCH_DB=~/.cc-switch/cc-switch.db TELEMETRY_SERVER_URL=http://127.0.0.1:8787 \
  TELEMETRY_NODE_ID=node-a cargo run -p telemetry-client
```

Server 使用 `TELEMETRY_DB`、`TELEMETRY_LISTEN`、`TELEMETRY_TOKEN` 环境变量。Client 使用 `CC_SWITCH_DB`、`TELEMETRY_SERVER_URL`、`TELEMETRY_NODE_ID`、`TELEMETRY_TOKEN`。Client 默认将游标保存到 `./data/client-cursor.json`，可通过 `TELEMETRY_STATE` 覆盖。

Server 默认监听 `127.0.0.1:8787`。启动后在本机打开：

```text
http://127.0.0.1:8787/dashboard/
```

Dashboard 的 HTML、CSS 和 JavaScript 已编译进 Server 二进制，不需要 Node.js、CDN 或单独部署前端。

## Dashboard

Dashboard 提供：

- 请求数、真实 Token、预估费用、成功率、缓存命中率和平均延迟；
- 5 分钟、小时或本地自然日的使用趋势；
- 节点、应用、Provider 和模型 Top 10 分布；
- 时间、节点、应用、Provider、模型和数据来源组合筛选；
- 最近请求的稳定游标分页；
- 简体中文与英文切换；首次按浏览器语言选择，并持久化用户选择；
- 每 30 秒自动刷新，浏览器标签页隐藏时暂停。

页面和 `/v1/dashboard/*` API 只允许来自 loopback 地址的连接。即使
`TELEMETRY_LISTEN=0.0.0.0:8787` 用于接收远程 Client，远程客户端也不能直接访问 Dashboard。
需要从另一台机器查看时，通过 SSH 转发：

```bash
ssh -L 8787:127.0.0.1:8787 user@server-host
```

然后在本机访问 `http://127.0.0.1:8787/dashboard/`。不要通过未认证的反向代理暴露 Dashboard；
服务端刻意忽略 `X-Forwarded-For`，本机反向代理会被视为 loopback。

## 数据边界

- `proxy_request_logs.created_at` 是 Unix epoch 秒；Client 使用 `(created_at, request_id)` 复合游标。
- 所有上传事件以 `node_id + ':' + request_id` 幂等。
- Client 每 5 秒比较 `cc-switch.db` 和 `cc-switch.db-wal` 的修改时间与大小；有变化立即同步，无变化不查询数据库。
- Client 每次变化后回看 10 分钟，容忍会话日志的迟到写入；满 500 条时连续读取下一批，不在历史回填批次间等待。
- Client 对短暂的连接失败以及 HTTP 408、425、429、500、502、503、504 使用指数退避重试；只有服务确认 batch 后才推进游标。
- Client 使用 reqwest 的默认代理发现逻辑，遵循 `HTTP_PROXY`/`HTTPS_PROXY` 与 `NO_PROXY`/`no_proxy`；不对目标地址做硬编码绕过。
- Server 通过有界写入队列和单一后台 worker 串行处理 event/rollup SQLite 写入；队列满时等待最多 30 秒，只有入队超时、worker 不可用或写入处理超时才返回 503。
- 同步日志分别报告 `sent`、`accepted`、`duplicates`、`rejected`；Server 重复写入不会重复计费。
- `usage_daily_rollups` 的 snapshot 接口预留给已从 cc-switch detail 删除的完整历史日；不能把 detail 与同一日期 rollup 直接相加。
- Dashboard 明确采用 **detail-only** 统计，只查询 Server 的 `usage_events`，不合并
  `usage_daily_snapshots`；页面会显示当前筛选实际覆盖的首末事件时间。
- `input_token_semantics` 会随 detail event 上传；旧 cc-switch schema 或旧事件默认按 legacy
  语义处理。Server 启动时会幂等迁移旧数据库。
- 真实 Token 和缓存命中率沿用 cc-switch 的 cache-normalization 口径；成功请求定义为 HTTP 2xx。
- 页面费用是上传事件中本地定价产生的估算值，不是 Provider 账单。
- 上传内容不包含 API key、prompt、response body 或原始会话文本。

## API

- `GET /healthz`
- `POST /v1/events/batch`
- `POST /v1/rollups/snapshot`
- `GET /v1/usage/summary`
- `GET /dashboard/`（仅 loopback）
- `GET /v1/dashboard/overview`（仅 loopback）
- `GET /v1/dashboard/filters`（仅 loopback）
- `GET /v1/dashboard/events`（仅 loopback）

Dashboard 查询公共参数为：

- `from`、`to`：Unix 秒，范围为 `[from, to)`；默认最近 24 小时，最多 365 天；
- `node_id`、`app_type`、`provider_id`、`model`、`data_source`：可选精确筛选；
- overview 额外支持 `bucket=auto|1s|1m|5m|1h|1d` 和 `tz_offset_minutes`；`auto` 会按筛选范围选择至少 10 个零填充数据点的最大粒度，并在 response 的 `range.bucket` 返回实际粒度；
- overview 的 `trend` 覆盖完整 `[from, to)` 时间轴，没有事件的 bucket 返回全零指标；趋势点同时提供 `input_tokens`、`fresh_input_tokens`、`cache_creation_tokens`、`cache_read_tokens` 和 `output_tokens`；
- events 支持 `limit`（默认 50、最多 200）以及成对出现的
  `before_created_at`、`before_event_id` 游标。

`TELEMETRY_TOKEN` 继续保护摄入接口及原有 summary API，不会发送到浏览器。
Dashboard 依靠 loopback 访问边界，不提供网页登录。

Server 是中心 SQLite 的唯一写入者；不要让多个节点直接打开共享网络 SQLite 文件。
