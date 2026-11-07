import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
const Store = Context.Service('test/ThenableResolvesService');
const deferred = Effect.runSync(
  Effect.provide(
    Effect.map(Store, (store) => ({
      then(resolve) {
        resolve(store);
      },
    })),
    Layer.succeed(Store)({ resolvedUsed: () => 'used', resolvedUnused: () => 'unused' }),
  ),
);
const store = await deferred;
assert.strictEqual(store.resolvedUsed(), 'used');
