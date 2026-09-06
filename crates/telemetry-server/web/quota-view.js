export function quotaMetricIdentity(metric) {
  return JSON.stringify([metric.key || "", metric.kind || "", metric.unit || ""]);
}

export function optionalQuotaNumber(value) {
  if (value == null || value === "") return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

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
