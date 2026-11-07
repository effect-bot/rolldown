import assert from 'node:assert/strict';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';

// A service with no `then` is returned from a plain `async` function and
// awaited. Awaiting reads `then` and finds none, so the service is not a
// hand-written thenable and nothing bails: the method the caller calls stays,
// and `asyncUnused` is removed.
const Store = Context.Service('test/AsyncReturnServicePrecise');
const store = Effect.runSync(
  Effect.provideService(Store, Store, {
    asyncUsed: () => 'used',
    asyncUnused: () => 'unused',
  }),
);
const load = async () => store;
assert.equal((await load()).asyncUsed(), 'used');
