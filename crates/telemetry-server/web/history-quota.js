import { estimatedQuotaCycles } from "./estimated-quota.js";
import { providerIdentity, metricIdentity } from "./quota-settings.js";

export const historyIdentity = ({ provider, metric }) => JSON.stringify([
  providerIdentity(provider), metricIdentity(metric),
]);

export function historyCandidates(cycles, config, range, now) {
  const ended = cycles.filter((cycle) => cycle.to <= now).sort((a, b) => b.from - a.from);
  if (config.mode === "cycle") return ended.filter((cycle) => cycle.resetsAt === Number(config.cycleId));
  const at = Math.min(range.to - 1, now);
  const current = cycles.find((cycle) => cycle.from <= at && at < cycle.to);
  if (!current) return [];
  const previous = ended.filter((cycle) => cycle.to <= current.from);
  return config.mode === "previous" ? previous.slice(0, 1) : previous;
}

function scopeParams(context) {
  const params = new URLSearchParams(context.params);
  for (const key of ["from", "to", "bucket", "all_time"]) params.delete(key);
  return params;
}

export function historyRequestKey(context, config) {
  return JSON.stringify([historyIdentity(context), config, scopeParams(context).toString(),
    context.billing, ["previous", "full"].includes(config.mode) ? context.range : null]);
}

// Cache fulfilled responses only: each caller owns cancellation independently.
export function createHistoryQuotaService(fetchJson, postJson, now = Date.now) {
  const cache = new Map();
  let revision = 0;
  async function cached(key, run, signal) {
    const hit = cache.get(key);
    if (hit && now() - hit.at < 300000) return hit.value;
    const current = revision;
    const value = await run();
    if (!signal.aborted && current === revision) {
      cache.set(key, { at: now(), value });
      while (cache.size > 256) cache.delete(cache.keys().next().value);
    }
    return value;
  }
  return {
    invalidate() { revision += 1; cache.clear(); },
    async resolve(context, config, signal) {
      if (config.mode === "none") return { cycles: [], reference: null, reason: "disabled" };
      if (config.mode === "manual") {
        const amount = Number(config.amount);
        return { cycles: [], reference: Number.isFinite(amount) && amount > 0 ? { amount, mode: "manual" } : null,
          reason: "invalidAmount" };
      }
      const { provider, metric, range } = context;
      const identity = historyIdentity(context);
      const query = new URLSearchParams({ node_id: provider.nodeId, provider_id: provider.providerId,
        metric_key: metric.key, metric_kind: metric.kind, unit: metric.unit || "" });
      const history = await cached(`resets:${identity}`, () => fetchJson(`/v3/dashboard/quota/resets?${query}`, signal), signal);
      if (signal.aborted) throw new DOMException("Aborted", "AbortError");
      const all = estimatedQuotaCycles(history, provider, metric, { from: 0, to: Infinity });
      const seconds = Math.floor(now() / 1000);
      const cycles = all.filter((cycle) => cycle.to <= seconds).sort((a, b) => b.from - a.from);
      if (config.mode === "cycle" && config.identity !== identity) return { cycles, reference: null, reason: "chooseCycle" };
      const candidates = historyCandidates(all, config, range, seconds);
      let selected;
      for (let index = 0; index < candidates.length && !selected; index += 128) {
        const batch = candidates.slice(index, index + 128);
        const body = { nodeId: provider.nodeId, providerId: provider.providerId,
          metricKey: metric.key, metricKind: metric.kind, unit: metric.unit || null, cycles: batch };
        const response = await cached(`summaries:${JSON.stringify(body)}`, () => postJson(
          "/v3/dashboard/quota/cycle-summaries", body, signal), signal);
        if (signal.aborted) throw new DOMException("Aborted", "AbortError");
        selected = response.cycles.find((cycle) => config.mode !== "full" || cycle.reachedFull);
      }
      if (!selected || !(selected.utilizationPercent > 0) || !Number.isSafeInteger(selected.sampledAt)) {
        return { cycles, reference: null, reason: "unavailable" };
      }
      const params = scopeParams(context);
      params.set("from", selected.from);
      params.set("to", Math.min(selected.to, selected.sampledAt + 1));
      params.set("bucket", "auto");
      const overview = await cached(`cost:${params}:${JSON.stringify(context.billing)}`, () => fetchJson(
        `/v3/dashboard/overview?${params}`, signal), signal);
      const cost = Number(overview.summary.totalCostUsd);
      const amount = cost / (selected.utilizationPercent / 100);
      return { cycles, reason: "unavailable", reference: Number.isFinite(amount) && amount > 0
        ? { ...selected, cost, amount, mode: config.mode } : null };
    },
  };
}

export function createHistoryQuotaLoader(service, onChange) {
  let generation = 0, controller, key;
  let state = { loading: false, reference: null, cycles: [] };
  return {
    cancel() { generation += 1; controller?.abort(); key = null; state = { loading: false, reference: null, cycles: [] }; },
    ensure(context, config) {
      const next = historyRequestKey(context, config);
      if (key === next) return state;
      controller?.abort(); controller = new AbortController();
      const current = ++generation;
      key = next;
      state = { loading: true, reference: null, cycles: [] };
      service.resolve(context, config, controller.signal).then((result) => {
        if (current !== generation) return;
        state = { ...result, loading: false }; onChange();
      }).catch((error) => {
        if (current !== generation || error.name === "AbortError") return;
        state = { loading: false, reference: null, cycles: [], reason: "failed" }; onChange();
      });
      return state;
    },
  };
}
