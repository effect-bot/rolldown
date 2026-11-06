import { dual } from 'effect/Function';

// Each body enumerates `self`, the data argument, and calls one method on the
// service it is given as `that`.
export const describeTwo = dual(2, (self, that) => [Object.values(self).join(','), that.twoUsed()]);

export const describeThree = dual(3, (self, that, label) => [
  label,
  Object.values(self).join(','),
  that.threeUsed(),
]);

// Predicate style: a single argument that is not a string is the data, so
// `runAll(service)` is a full call that enumerates the service.
export const runAll = dual(
  (args) => typeof args[0] !== 'string',
  (self, prefix = '') => Object.values(self).map((method) => prefix + method()),
);
