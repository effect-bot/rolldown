import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
const Store = Context.Service('test/AsyncReturnThenable');
const service = {
  adoptUsed: () => 'done',
  adoptUnused: () => 'unused',
  then(resolve) {
    const self = this;
    resolve({
      then(inner) {
        inner(self.adoptUsed());
      },
    });
  },
};
const load = Effect.runSync(
  Effect.provideService(
    Effect.map(Store, (store) => async () => store),
    Store,
    service,
  ),
);
assert.strictEqual(await load(), 'done');
