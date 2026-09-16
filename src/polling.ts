/** One timer and one request at a time. Visibility generations discard stale replies. */
export function visibilityPoll<T>(read: () => Promise<T>, accept: (value: T) => void,
  failed: (error: unknown) => void, reset: () => void, interval = 1000) {
  let visible = false, disposed = false, generation = 0, pending = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function poll() {
    if (!visible || disposed || pending) return;
    pending = true;
    const stamp = generation;
    try { const value = await read(); if (!disposed && visible && stamp === generation) accept(value); }
    catch (error) { if (!disposed && visible && stamp === generation) failed(error); }
    finally {
      pending = false;
      if (visible && !disposed) timer = setTimeout(poll, stamp === generation ? interval : 0);
    }
  }
  return {
    visibility(next: boolean) {
      if (disposed || next === visible) return;
      visible = next; generation++; clearTimeout(timer); reset();
      if (visible) void poll();
    },
    dispose() { disposed = true; generation++; clearTimeout(timer); },
  };
}
