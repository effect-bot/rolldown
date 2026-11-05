import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A service with a `then` method is resolved by a Promise twice: returned from
// an `async` function, and as the success of `Effect.runPromise`. Promise
// resolution calls `then`, and the program observes the value it resolves
// with, so `then` must stay. `thenResolvedUnused` is never read.
const Store = Context.Service('test/ThenResolved');
const thenResolvedUnusedHelper = () => 'unused';
let thenCalls = 0;
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    thenResolvedRead: () => 'read',
    thenResolvedUnused: () => thenResolvedUnusedHelper(),
    then(resolve) {
      thenCalls++;
      resolve({ unwrapped: thenCalls });
    },
  }),
);

const service = Effect.gen(function* () {
  const store = yield* Store;
  return store;
});

const fromAsync = async () => {
  const store = await Effect.runPromise(
    Effect.provide(
      Effect.map(service, (s) => [s]),
      StoreLayer,
    ),
  );
  return store[0];
};

const read = await Effect.runPromise(
  Effect.provide(
    Effect.map(service, (s) => s.thenResolvedRead()),
    StoreLayer,
  ),
);
const viaAsync = await fromAsync();
const viaRunPromise = await Effect.runPromise(Effect.provide(service, StoreLayer));
assert.deepStrictEqual(
  [read, viaAsync, viaRunPromise, thenCalls],
  ['read', { unwrapped: 1 }, { unwrapped: 2 }, 2],
);
