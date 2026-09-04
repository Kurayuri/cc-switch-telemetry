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
  const gapSeconds = Math.max(90, Number(bucketSeconds || 60) * 1.5);
  const segments = [];
  let segment = [];
  let previous = null;
  for (const point of points || []) {
    if (valueSelector(point) == null) {
      if (segment.length) segments.push(segment);
      segment = [];
      previous = null;
      continue;
    }
    if (previous != null && point.sampledAt - previous > gapSeconds) {
      if (segment.length) segments.push(segment);
      segment = [];
    }
    segment.push(point);
    previous = point.sampledAt;
  }
  if (segment.length) segments.push(segment);
  return segments;
}
