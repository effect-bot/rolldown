import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Inspectable from 'effect/Inspectable';
const Store = Context.Service('test/ThenableResolveNested');
const store = {
  resolveUsed: () => 'nested',
  resolveUnused: () => 'unused',
  then(resolve) {
    const self = this;
    resolve({
      then(inner) {
        inner(self.resolveUsed());
      },
    });
  },
};
const program = Effect.gen(function* () {
  const service = yield* Store;
  return async () => Inspectable.toJson(service);
});
const resolve = await Effect.runPromise(Effect.provideService(program, Store, store));
assert.strictEqual(await resolve(), 'nested');
