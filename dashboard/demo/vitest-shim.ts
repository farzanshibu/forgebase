// Demo mode: the two bits of vitest the test fakes use (stubGlobal, fn), so they run in the browser.
type Fn = (...a: never[]) => unknown;
export const vi = {
  stubGlobal(name: string, value: unknown) {
    (globalThis as Record<string, unknown>)[name] = value;
  },
  fn<T extends Fn>(f?: T) {
    return (f ?? (() => undefined)) as T;
  },
};
export const describe = () => {};
export const it = () => {};
export const expect = () => ({});
