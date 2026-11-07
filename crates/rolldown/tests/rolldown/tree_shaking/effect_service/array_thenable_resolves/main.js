import assert from 'node:assert/strict';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
const Store = Context.Service('test/ArrayThenableResolves');
const store = Effect.runSync(
  Effect.provideService(Store, Store, {
    arrayThenUsed: () => 'used',
    arrayThenUnused: () => 'unused',
  }),
);
const deferred = Object.assign([], {
  then(resolve) {
    resolve(store);
  },
});
assert.equal((await deferred).arrayThenUsed(), 'used');
