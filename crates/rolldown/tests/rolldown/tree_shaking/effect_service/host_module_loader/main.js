import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { getBuiltinModule } from 'node:process';

// The service is handed to code compiled at run time by a loader reached through
// `process.getBuiltinModule('vm')`. The pass cannot see that code, so the
// service's key keeps every method.
const Store = Context.Service('test/RealHostModuleLoader');
const hostLoaderUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const hostLoaderUnused = () => hostLoaderUnusedHelper();
    return Store.of({ readOne, hostLoaderUnused });
  }),
);

const observe = (store) =>
  getBuiltinModule('vm').compileFunction('return store.hostLoaderUnused()', ['store'])(store);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), observe(store)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
