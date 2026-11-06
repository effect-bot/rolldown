import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { compileFunction } from 'node:vm';

// Retention: the service is handed to code compiled by `node:vm`, which calls a
// method of it. A service handed to code the pass cannot see bails its key, so
// `vmUnused` stays.
const Store = Context.Service('test/RealNodeVm');

const vmUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const vmUnused = () => vmUnusedHelper();
    return Store.of({ readOne, vmUnused });
  }),
);

const observe = (store) => compileFunction('return store.vmUnused()', ['store'])(store);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), observe(store)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
