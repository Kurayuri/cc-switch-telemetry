import { quotaTierPeriodLabel, quotaTierPeriodSeconds } from "./quota-settings.js";

export function quotaMetricIdentity(metric) {
  return JSON.stringify([metric.key || "", metric.kind || "", metric.unit || ""]);
}

export function optionalQuotaNumber(value) {
  if (value == null || value === "") return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

export function quotaResetTiers(provider) {
  const series = new Map((provider?.series || [])
    .map((item) => [quotaMetricIdentity(item), item]));
  return (provider?.current || []).flatMap((metric) => {
    const resetsAt = optionalQuotaNumber(metric.resetsAt);
    const sampledAt = optionalQuotaNumber(metric.sampledAt);
    if (!Number.isSafeInteger(resetsAt) || resetsAt <= 0
      || (sampledAt != null && resetsAt <= sampledAt)) return [];
    const periodSeconds = quotaTierPeriodSeconds(metric, series.get(quotaMetricIdentity(metric))?.points);
    if (!Number.isSafeInteger(periodSeconds) || periodSeconds <= 0
      || periodSeconds > 720 * 24 * 60 * 60) return [];
    return [{
      id: quotaMetricIdentity(metric),
      metric,
      periodSeconds,
      periodLabel: quotaTierPeriodLabel(periodSeconds),
    }];
  });
}

export { quotaTierPeriodLabel, quotaTierPeriodSeconds };

function boundedPercentage(value) {
  return Math.max(0, Math.min(100, value));
}

export function quotaPercentage(metric) {
  const explicit = optionalQuotaNumber(metric?.utilizationPercent);
  if (explicit != null) return boundedPercentage(explicit);

  const total = optionalQuotaNumber(metric?.total);
  if (total == null || total <= 0) return null;
  const used = optionalQuotaNumber(metric?.used);
  if (used != null) return boundedPercentage(used / total * 100);
  const remaining = optionalQuotaNumber(metric?.remaining);
  if (remaining != null) return boundedPercentage((total - remaining) / total * 100);
  return null;
}

export function quotaAmount(metric) {
  if (quotaPercentage(metric) != null) return null;
  if (optionalQuotaNumber(metric?.remaining) != null) {
    return optionalQuotaNumber(metric.remaining);
  }
  if (optionalQuotaNumber(metric?.used) != null) return optionalQuotaNumber(metric.used);
  return optionalQuotaNumber(metric?.total);
}

export function quotaMetricValue(metric) {
  return quotaPercentage(metric) ?? quotaAmount(metric);
}

export function quotaAmountRange(values) {
  const finiteValues = (values || []).map(optionalQuotaNumber).filter((value) => value != null);
  const minimum = Math.min(0, ...finiteValues);
  let maximum = Math.max(0, ...finiteValues);
  if (maximum === minimum) maximum = minimum + 1;
  return { minimum, maximum, span: maximum - minimum };
}

export function filterQuotaProviders(providers, nodeId, providerId) {
  return (providers || []).filter((provider) => {
    if (nodeId && provider.nodeId !== nodeId) return false;
    return !providerId || provider.providerId === providerId;
  });
}

export function splitQuotaPoints(points, bucketSeconds, valueSelector = quotaMetricValue) {
  const segments = [];
  let segment = [];
  let previous = null;
  for (const point of points || []) {
    const value = optionalQuotaNumber(valueSelector(point));
    if (value == null) {
      // A different valid scale is a boundary; an absent sample is not.
      if (quotaMetricValue(point) != null) {
        if (segment.length) segments.push(segment);
        segment = [];
        previous = null;
      }
      continue;
    }
    const hasSegments = previous?.segmentId != null && point.segmentId != null;
    if (previous && (hasSegments
      ? previous.segmentId !== point.segmentId
      : point.sampledAt - previous.sampledAt > 600)) {
      if (segment.length) segments.push(segment);
      segment = [];
    }
    segment.push(point);
    previous = point;
  }
  if (segment.length) segments.push(segment);
  return segments;
}

// Platform starts retain the time spent at an unchanged value between changes.
export function quotaPrediction(points, bucketSeconds, valueSelector = quotaMetricValue) {
  const latest = (points || []).findLast((point) => quotaMetricValue(point) != null);
  if (!latest || optionalQuotaNumber(valueSelector(latest)) == null) return null;
  const segment = splitQuotaPoints(points, bucketSeconds, valueSelector).at(-1) || [];
  let previous = null;
  let platforms = [];
  for (const point of segment) {
    const at = optionalQuotaNumber(point.sampledAt);
    const value = optionalQuotaNumber(valueSelector(point));
    if (at == null || value == null) { previous = null; platforms = []; continue; }
    const basis = quotaPercentage(point) != null ? "percent"
      : optionalQuotaNumber(point.remaining) != null ? "remaining"
        : optionalQuotaNumber(point.used) != null ? "used" : "total";
    const reset = optionalQuotaNumber(point.resetsAt);
    if (previous && (at <= previous.at || basis !== previous.basis
      || (reset != null && previous.reset != null && Math.abs(reset - previous.reset) > 2)
      || (previous.reset != null && at >= previous.reset)
      || (basis === "remaining" ? value > previous.value : value < previous.value))) {
      platforms = [];
    }
    if (!platforms.length || value !== platforms.at(-1).value) {
      platforms.push({ at, value });
      if (platforms.length > 2) platforms.shift();
    }
    previous = { at, value, basis, reset };
  }
  if (!previous || platforms.length < 2) return null;
  const [a, b] = platforms;
  const slope = (b.value - a.value) / (b.at - a.at);
  if (!Number.isFinite(slope) || slope === 0) return null;
  const { at, value, basis, reset } = previous;
  if (reset != null && reset <= at) return null;
  const limit = basis === "percent" && slope > 0 ? 100
    : basis === "remaining" && slope < 0 ? 0 : null;
  const exhaustedAt = limit == null ? Infinity : at + (limit - value) / slope;
  const endAt = Math.min(exhaustedAt, reset ?? Infinity);
  if (!(endAt > at) || !Number.isFinite(new Date(endAt * 1000).getTime())) return null;
  const endValue = endAt === exhaustedAt ? limit : value + slope * (endAt - at);
  if (!Number.isFinite(endValue)) return null;
  return {
    start: { at, value }, end: { at: endAt, value: endValue }, slope,
    exhaustedAt: Number.isFinite(new Date(exhaustedAt * 1000).getTime()) ? exhaustedAt : null,
  };
}

export function quotaExhaustion(metric, prediction, { loading = false, error = null } = {}) {
  const reset = optionalQuotaNumber(metric?.resetsAt);
  const sampledAt = optionalQuotaNumber(metric?.sampledAt);
  if (!Number.isSafeInteger(reset) || reset <= 0
    || !Number.isFinite(new Date(reset * 1000).getTime())
    || (sampledAt != null && reset <= sampledAt)) return null;
  const percentage = quotaPercentage(metric);
  const remaining = optionalQuotaNumber(metric?.remaining);
  if (percentage === 100 || (percentage == null && remaining != null && remaining <= 0)) {
    return { status: "exhausted", beforeReset: sampledAt != null && sampledAt < reset };
  }
  const at = optionalQuotaNumber(prediction?.exhaustedAt);
  if (at != null && Number.isFinite(new Date(at * 1000).getTime())
    && (sampledAt == null || prediction.start.at === sampledAt)) {
    return { status: "estimated", at, beforeReset: at < reset, afterReset: at > reset };
  }
  return { status: loading && !error ? "loading" : "unavailable", beforeReset: false };
}

export function quotaPredictionLookback(metric, range) {
  const period = quotaTierPeriodSeconds(metric) || 30 * 86400;
  return Math.max(0, Math.min(Number(range.from), Number(range.to) - period));
}

export function quotaPredictionRange(range, predictions) {
  const end = Math.max(Number(range.to), ...predictions.map((prediction) => prediction?.end.at || 0));
  if (end <= Number(range.to)) return { ...range };
  // Keep the endpoint clear of the right border without changing the history query.
  const padding = Math.max(60, (end - Number(range.to)) * 0.05);
  return { ...range, to: Math.ceil(end + padding) };
}

export function quotaPredictionRows(plots, at, legendSelection = {}) {
  return plots.flatMap((plot) => {
    const prediction = plot.prediction;
    if (legendSelection[plot.name] === false || !prediction
      || at < prediction.start.at || at > prediction.end.at) return [];
    return [{ plot, value: prediction.start.value + prediction.slope * (at - prediction.start.at) }];
  });
}

export function quotaSnapshotRows(plots, snapshot, legendSelection = {}) {
  const identity = (p) => JSON.stringify([p.nodeId, p.providerId, p.key, p.kind, p.unit || ""]);
  const points = new Map((snapshot?.metrics || []).map((p) => [identity(p), p]));
  return plots.filter((plot) => legendSelection[plot.name] !== false).map((plot) => {
    const candidate = points.get(plot.metricId);
    const age = Number(snapshot?.at) - Number(candidate?.sampledAt);
    const point = candidate && age >= 0 && age <= 600 ? candidate : null;
    return { plot, point, value: point ? plot.value(point) : null };
  });
}

export function createQuotaSnapshotLoader(fetcher, delay = 100) {
  const cache = new Map();
  let generation = 0, timer = null, controller = null;
  const cancel = () => { generation++; clearTimeout(timer); controller?.abort(); controller = null; };
  return {
    peek: (at) => cache.get(at),
    cancel,
    invalidate: () => { cancel(); cache.clear(); },
    request(at, success, failure) {
      cancel();
      if (cache.has(at)) { success(cache.get(at)); return; }
      const current = generation;
      timer = setTimeout(async () => {
        const request = new AbortController();
        controller = request;
        try {
          const result = await fetcher(at, request.signal);
          if (current !== generation) return;
          cache.set(at, result);
          if (cache.size > 64) cache.delete(cache.keys().next().value);
          success(result);
        } catch (error) {
          if (current === generation && error.name !== "AbortError") failure(error);
        } finally { if (current === generation) controller = null; }
      }, delay);
    },
  };
}
