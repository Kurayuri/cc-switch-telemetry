import { MAX_RANGE_SECONDS } from "./range.js";
import { metricIdentity, providerIdentity, quotaTierPeriodSeconds, quotaTierPeriodLabel } from "./quota-settings.js";

export const RESET_CONFIRMATION_SAMPLES = 3;

// The server returns chronological consecutive runs with a fixed 60s anchor.
// A returning old reset is ignored even if it persists for many samples.
export function confirmedResetObservations(observations = []) {
  const confirmed = [];
  for (const record of [...observations].sort((a, b) => a.firstSampledAt - b.firstSampledAt)) {
    if (!Number.isSafeInteger(record.resetsAt) || record.resetsAt <= 0
      || !Number.isSafeInteger(record.firstSampledAt) || record.firstSampledAt < 0
      || !Number.isSafeInteger(record.lastSampledAt) || record.lastSampledAt < record.firstSampledAt
      || !Number.isSafeInteger(record.sampleCount) || record.sampleCount < 1) continue;
    const previous = confirmed.at(-1);
    if (previous && Math.abs(previous.resetsAt - record.resetsAt) <= 60) {
      previous.lastSampledAt = Math.max(previous.lastSampledAt, record.lastSampledAt);
      if (previous.firstUsageAt == null) previous.firstUsageAt = record.firstUsageAt;
      continue;
    }
    if (confirmed.some((item) => Math.abs(item.resetsAt - record.resetsAt) <= 60)) continue;
    // A fixed-length tier's newly starting window must advance in time.
    if (previous && record.resetsAt <= previous.resetsAt) continue;
    // The confirmation must outlast the jitter window, so rapid polling cannot
    // turn a continuously sliding deadline into a sequence of stable resets.
    if (record.sampleCount >= RESET_CONFIRMATION_SAMPLES
      && record.lastSampledAt - record.firstSampledAt > 60) confirmed.push({ ...record });
  }
  return confirmed;
}

export function resolvePastResetRange(cycle, nowMs = Date.now()) {
  if (!cycle) return null;
  const { from, resetsAt } = cycle;
  const endAt = cycle.endAt ?? resetsAt;
  const to = Math.min(endAt, Math.floor(nowMs / 1000));
  return Number.isSafeInteger(from) && Number.isSafeInteger(endAt)
    && Number.isSafeInteger(to) && from >= 0 && from < to
    && endAt - from <= MAX_RANGE_SECONDS ? { from, to } : null;
}

export function pastResetGroups(response, nowMs = Date.now()) {
  return (response?.providers || []).map((provider) => ({
    provider,
    id: providerIdentity(provider),
    tiers: (provider.tiers || []).map((metric) => {
      const resets = confirmedResetObservations(metric.resets);
      const periodSeconds = quotaTierPeriodSeconds(metric, resets);
      const cycles = Number.isSafeInteger(periodSeconds) && periodSeconds > 0
        && periodSeconds <= MAX_RANGE_SECONDS ? resets.map((record, index) => ({
          ...record, id: String(record.resetsAt), from: record.resetsAt - periodSeconds,
          endAt: Math.min(record.resetsAt, resets[index + 1] ? resets[index + 1].resetsAt - periodSeconds : Infinity),
        })).filter((cycle) => resolvePastResetRange(cycle, nowMs)
          // Usage is a display filter, never a reset-transition signal.
          && Number.isSafeInteger(cycle.firstUsageAt) && cycle.firstUsageAt >= cycle.from
          && cycle.firstUsageAt < cycle.endAt).reverse() : [];
      return { id: metricIdentity(metric), metric, periodSeconds,
        periodLabel: quotaTierPeriodLabel(periodSeconds), cycles };
    }).filter((tier) => tier.cycles.length),
  })).filter((group) => group.tiers.length);
}

export function choosePastReset(groups, selection) {
  const group = groups.find((item) => item.id === selection?.providerKey) || groups[0];
  const tier = group?.tiers.find((item) => item.id === selection?.tierId) || group?.tiers[0];
  const cycle = tier?.cycles.find((item) => item.id === selection?.cycleId) || tier?.cycles[0];
  return cycle ? { providerKey: group.id, tierId: tier.id, cycleId: cycle.id, cycle } : null;
}

// Only the latest open picker may publish a response. Aborting is an optimization;
// generation checks also handle fetch implementations which ignore AbortSignal.
export function createPastResetLoader(fetchHistory, onChange) {
  let generation = 0;
  let controller;
  return {
    async load() {
      const current = ++generation;
      controller?.abort();
      controller = new AbortController();
      onChange({ loading: true, error: false, response: null });
      try {
        const response = await fetchHistory(controller.signal);
        if (current === generation) onChange({ loading: false, error: false, response });
      } catch (error) {
        if (current === generation && error.name !== "AbortError") {
          onChange({ loading: false, error: true, response: null });
        }
      }
    },
    cancel() { generation += 1; controller?.abort(); },
  };
}
