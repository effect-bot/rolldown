// One export is used; the other loads code but nothing imports it.
export const double = (value) => value * 2;

export const neverCalled = () => new Function('return 1')();
