import { confirmedResetObservations, resolvePastResetRange } from "./past-resets.js";
import { quotaResetTiers } from "./quota-view.js";
import { providerIdentity, metricIdentity, quotaTierPeriodSeconds, quotaTierPeriodLabel } from "./quota-settings.js";
import { MAX_RANGE_SECONDS } from "./range.js";

// Unlike historical quota references, the range picker includes zero-use cycles.
export function resetCycleGroups(history, liveProviders = [], nowMs = Date.now()) {
  const groups = new Map();
  const now = Math.floor(nowMs / 1000);
  for (const provider of history?.providers || []) {
    const tiers = (provider.tiers || []).flatMap(metric => {
      const resets = confirmedResetObservations(metric.resets);
      const periodSeconds = quotaTierPeriodSeconds(metric, resets);
      if (!Number.isSafeInteger(periodSeconds) || periodSeconds <= 0 || periodSeconds > MAX_RANGE_SECONDS) return [];
      const cycles = resets.map((record, index) => ({
        ...record, id: String(record.resetsAt), from: record.resetsAt - periodSeconds,
        endAt: Math.min(record.resetsAt, resets[index + 1] ? resets[index + 1].resetsAt - periodSeconds : Infinity),
      })).filter(cycle => resolvePastResetRange(cycle, nowMs)).reverse();
      return [{ id: metricIdentity(metric), metric, periodSeconds,
        periodLabel: quotaTierPeriodLabel(periodSeconds), cycles }];
    });
    groups.set(providerIdentity(provider), { id: providerIdentity(provider), provider, tiers });
  }
  for (const provider of liveProviders) {
    const id = providerIdentity(provider);
    const group = groups.get(id) || { id, provider, tiers: [] };
    group.provider = provider;
    for (const live of quotaResetTiers(provider)) {
      let tier = group.tiers.find(item => item.id === live.id);
      if (!tier) { tier = { ...live, cycles: [] }; group.tiers.push(tier); }
      // Confirmed boundaries win over live deadline jitter. A fresh current-only
      // sample may initialize the current cycle before historical confirmation.
      const reset = live.metric.resetsAt, from = reset - live.periodSeconds;
      if (!tier.cycles.some(c => c.from <= now && now < c.endAt)
        && Number.isSafeInteger(reset) && from < now && now < reset
        && !tier.cycles.some(c => c.from > from)) {
        tier.cycles.unshift({ id: String(reset), from, endAt: reset, resetsAt: reset, provisional: true });
      }
    }
    if (group.tiers.length) groups.set(id, group);
  }
  return [...groups.values()].filter(group => group.tiers.length);
}

export function resolveResetCycleChoice(groups, selection, nowMs = Date.now()) {
  const group = selection?.providerKey ? groups.find(g => g.id === selection.providerKey) : groups[0];
  const tier = selection?.tierId ? group?.tiers.find(t => t.id === selection.tierId) : group?.tiers[0];
  const mode = selection?.mode || "current";
  const now = Math.floor(nowMs / 1000);
  const current = tier?.cycles.find(c => c.from <= now && now < c.endAt);
  let cycle;
  if (mode === "manual") cycle = tier?.cycles.find(c => c.id === selection?.cycleId);
  else if (mode === "current") cycle = current;
  else if (mode === "last" && current) {
    cycle = tier.cycles.find(c => c !== current && !c.provisional && c.endAt <= now
      && c.from < current.from && Math.abs(c.endAt - current.from) <= 60);
  }
  return { group, tier, mode, cycle, range: resolvePastResetRange(cycle, nowMs),
    selection: group && tier ? { providerKey: group.id, tierId: tier.id, mode,
      cycleId: cycle?.id ?? selection?.cycleId ?? null, cycle: cycle || null } : null };
}
