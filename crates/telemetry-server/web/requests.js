// Share in-flight GETs, but keep cancellation local to each subscriber.
export function createRequestPool(load) {
  const pending = new Map();
  return (url, signal) => {
    if (signal?.aborted) return Promise.reject(new DOMException("Aborted", "AbortError"));
    let entry = pending.get(url);
    if (!entry) {
      const controller = new AbortController();
      entry = { controller, users: 0 };
      pending.set(url, entry);
      entry.promise = Promise.resolve().then(() => load(url, controller.signal)).finally(() => {
        if (pending.get(url) === entry) pending.delete(url);
      });
    }
    entry.users += 1;
    return new Promise((resolve, reject) => {
      let done = false;
      const finish = (callback, value) => {
        if (done) return;
        done = true;
        signal?.removeEventListener("abort", abort);
        entry.users -= 1;
        if (!entry.users && pending.get(url) === entry) {
          pending.delete(url);
          entry.controller.abort();
        }
        callback(value);
      };
      const abort = () => finish(reject, new DOMException("Aborted", "AbortError"));
      signal?.addEventListener("abort", abort, { once: true });
      entry.promise.then(value => finish(resolve, value), error => finish(reject, error));
    });
  };
}
