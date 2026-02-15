function safeEpoch(value) {
  if (!value) {
    return 0;
  }
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

export function pruneLocalOperationCaches(localOperations, localRequestIndex, options = {}) {
  const now = Number.isFinite(options.nowMs) ? options.nowMs : Date.now();
  const ttlMs = Math.max(60_000, Number.parseInt(String(options.ttlMs ?? 24 * 60 * 60 * 1000), 10) || 24 * 60 * 60 * 1000);
  const maxEntries = Math.max(1, Number.parseInt(String(options.maxEntries ?? 5000), 10) || 5000);

  for (const [name, operation] of localOperations.entries()) {
    const createEpoch = safeEpoch(operation?.metadata?.createTime);
    if (createEpoch > 0 && now - createEpoch > ttlMs) {
      localOperations.delete(name);
    }
  }

  if (localOperations.size > maxEntries) {
    const ordered = [...localOperations.entries()].sort(
      (left, right) => safeEpoch(left[1]?.metadata?.createTime) - safeEpoch(right[1]?.metadata?.createTime),
    );
    const toDrop = localOperations.size - maxEntries;
    for (let idx = 0; idx < toDrop; idx += 1) {
      if (ordered[idx]) {
        localOperations.delete(ordered[idx][0]);
      }
    }
  }

  for (const [requestKey, operationName] of localRequestIndex.entries()) {
    if (!localOperations.has(operationName)) {
      localRequestIndex.delete(requestKey);
    }
  }

  if (localRequestIndex.size > maxEntries) {
    const keys = [...localRequestIndex.keys()];
    const toDrop = localRequestIndex.size - maxEntries;
    for (let idx = 0; idx < toDrop; idx += 1) {
      localRequestIndex.delete(keys[idx]);
    }
  }
}
