import { confirmedResetObservations } from "./past-resets.js";
import { metricIdentity, providerIdentity, quotaTierPeriodSeconds } from "./quota-settings.js";
import { estimatedQuota, quotaPercentage } from "./quota-view.js";
import { MAX_RANGE_SECONDS } from "./range.js";

export function estimatedQuotaCycles(history, provider, metric, range) {
  const tier = history?.providers?.find((item) => providerIdentity(item) === providerIdentity(provider))
    ?.tiers?.find((item) => metricIdentity(item) === metricIdentity(metric));
  const resets = confirmedResetObservations(tier?.resets);
  const period = quotaTierPeriodSeconds(metric, resets);
  if (!Number.isSafeInteger(period) || period <= 0 || period > MAX_RANGE_SECONDS) return [];
  return resets.map((reset, index) => ({
    from: reset.resetsAt - period, resetsAt: reset.resetsAt,
    to: Math.min(reset.resetsAt, resets[index + 1] ? resets[index + 1].resetsAt - period : Infinity),
  })).filter((cycle) => cycle.from >= 0 && cycle.from < cycle.to
    && cycle.from < range.to && cycle.to > range.from);
}

function bucketEnd(start, bucket) {
  if (bucket === "1mo") {
    // Month buckets are UTC-aligned by the overview endpoint.
    const date = new Date(start * 1000);
    return Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 1) / 1000;
  }
  const match = /^(\d+)(s|m|h|d)$/.exec(bucket);
  return match ? start + Number(match[1]) * ({ s: 1, m: 60, h: 3600, d: 86400 })[match[2]] : NaN;
}

// The numerator is bucket-granular. Never interpolate a quota sample or carry
// one over an empty bucket. Both timestamps are retained for the tooltip.
export function estimatedQuotaPoints({ cycle, overview, prefixCost = 0, quotaPoints, range }) {
  const samples = quotaPoints.filter((point) => point.sampledAt >= cycle.from
    && point.sampledAt < cycle.to && Number.isFinite(point.resetsAt)
    && Math.abs(point.resetsAt - cycle.resetsAt) <= 60).sort((a, b) => a.sampledAt - b.sampledAt);
  let cost = prefixCost;
  let cursor = 0;
  let previous = null;
  const result = [];
  for (const bucket of overview.trend) {
    cost += Number(bucket.totalCostUsd);
    const from = Math.max(bucket.bucketStart, overview.range.from, cycle.from);
    const at = Math.min(bucketEnd(bucket.bucketStart, overview.range.bucket), overview.range.to, cycle.to);
    let sample = null;
    while (cursor < samples.length && samples[cursor].sampledAt < at) {
      const candidate = samples[cursor++];
      if (candidate.sampledAt >= from) sample = candidate;
    }
    const value = sample ? estimatedQuota(sample, cost) : null;
    if (at < range.from || at > range.to || at <= from) continue;
    if (sample && previous && (sample.segmentId != null && previous.segmentId != null
      ? sample.segmentId !== previous.segmentId : sample.sampledAt - previous.sampledAt > 600)) {
      result.push({ at, value: null });
    }
    result.push({ at, value, cost, utilization: quotaPercentage(sample),
      sampledAt: sample?.sampledAt, cycleFrom: cycle.from, resetsAt: cycle.resetsAt });
    previous = value != null ? sample : null;
  }
  return result;
}

// Keep the last complete result visible while revalidating this selection.
// Closed cycles and prefix queries expire after five minutes; manual refresh
// invalidates them without discarding the plotted result.
export function createEstimatedQuotaLoader(fetchJson, onChange, now = Date.now) {
  const ttl = 5 * 60 * 1000;
  let generation = 0, validation = 0;
  let controller, historyCache;
  const queries = new Map(), cyclesCache = new Map();
  let state = { key: null, loading: false, error: false, points: [] };
  let fingerprint = null;
  const trim = (cache) => { while (cache.size > 256) cache.delete(cache.keys().next().value); };
  return {
    cancel() {
      generation += 1;
      controller?.abort();
      state = { key: null, loading: false, error: false, points: [] };
      fingerprint = null;
      historyCache = null;
      queries.clear(); cyclesCache.clear();
    },
    revalidate() {
      validation += 1;
      historyCache = null;
      queries.clear(); cyclesCache.clear();
    },
    ensure(key, { provider, metric, overview, params }) {
      if (state.key !== key) this.cancel();
      const signature = JSON.stringify([validation, overview.range,
        overview.trend?.map((p) => [p.bucketStart, p.totalCostUsd]), metric.points]);
      if (fingerprint === signature && (state.loading || state.error || now() - state.updatedAt < ttl)) return state;
      fingerprint = signature;
      const current = ++generation;
      controller?.abort();
      controller = new AbortController();
      const signal = controller.signal;
      const previous = state.points;
      state = { ...state, key, loading: true, error: false };
      const requestOverview = async (from, to, bucket, cacheable = true) => {
        const query = new URLSearchParams(params);
        query.delete("all_time");
        query.set("from", String(from)); query.set("to", String(to)); query.set("bucket", bucket);
        const url = `/v3/dashboard/overview?${query}`;
        const cached = queries.get(url);
        if (cacheable && cached && now() - cached.at < ttl) return cached.value;
        const value = await fetchJson(url, signal);
        if (current === generation && cacheable) { queries.set(url, { at: now(), value }); trim(queries); }
        return value;
      };
      void (async () => {
        try {
          // A changed reset deadline can identify a new cycle before the TTL.
          const anchor = metric.points?.at(-1)?.resetsAt;
          const history = historyCache && historyCache.anchor === anchor && now() - historyCache.at < ttl
            ? historyCache.value : await fetchJson("/v3/dashboard/quota/resets", signal);
          if (current !== generation) return;
          historyCache = { anchor, at: historyCache?.value === history ? historyCache.at : now(), value: history };
          const cycles = estimatedQuotaCycles(history, provider, metric, overview.range);
          const results = new Array(cycles.length);
          let next = 0;
          await Promise.all(Array.from({ length: Math.min(3, cycles.length) }, async () => {
            while (next < cycles.length && !signal.aborted && current === generation) {
              const index = next++;
              const cycle = cycles[index];
              const from = Math.max(cycle.from, overview.range.from), to = Math.min(cycle.to, overview.range.to);
              const samples = (metric.points || []).filter((p) => p.sampledAt >= from && p.sampledAt < to);
              const cycleKey = JSON.stringify([cycle, from, to, overview.range.bucket,
                (overview.trend || []).filter((p) => bucketEnd(p.bucketStart, overview.range.bucket) > from && p.bucketStart < to)
                  .map((p) => [p.bucketStart, p.totalCostUsd]), samples]);
              const cached = cyclesCache.get(cycleKey);
              if (cached && now() - cached.at < ttl) { results[index] = cached.points; continue; }
              const prefix = from > cycle.from ? await requestOverview(cycle.from, from, "auto") : null;
              if (signal.aborted || current !== generation) return;
              const visible = from === overview.range.from && to === overview.range.to
                ? overview : await requestOverview(from, to, overview.range.bucket, false);
              const points = estimatedQuotaPoints({ cycle, overview: visible,
                prefixCost: prefix ? Number(prefix.summary.totalCostUsd) : 0,
                quotaPoints: metric.points, range: overview.range });
              results[index] = points;
              if (current === generation) { cyclesCache.set(cycleKey, { at: now(), points }); trim(cyclesCache); }
            }
          }));
          if (current !== generation) return;
          const points = results.flatMap((points, index) => index
            ? [{ at: cycles[index].from, value: null }, ...points] : points);
          state = { key, loading: false, error: false, points, updatedAt: now() };
          onChange();
        } catch (error) {
          if (current !== generation) return;
          controller.abort();
          state = { key, loading: false, error: true, points: previous, updatedAt: now() };
          onChange();
        }
      })();
      return state;
    },
  };
}
