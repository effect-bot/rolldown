import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A code loader reached through non-constant names on the host `process`
// compiles code outside the bundle. That code receives nothing and its result
// is a number, so no service escapes and the unread method is removed.
const Store = Context.Service('test/RealLoaderComputedAccess');

const computedAccessUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const computedAccessUnused = () => computedAccessUnusedHelper();
    return Store.of({ readOne, computedAccessUnused });
  }),
);

const load = () =>
  process[['getBuiltin', 'Module'].join('')]('vm')[['compile', 'Function'].join('')]('return 1')();

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), load()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['read', 1]);
