// Demo mode: src/test/helpers.tsx imports testing-library for the tests; the demo never calls it.
const unavailable = () => {
  throw new Error("testing-library is not available in demo mode");
};
export const render = unavailable;
export const screen = {};
export const within = unavailable;
export const waitFor = unavailable;
export const act = unavailable;
export default { setup: unavailable };
