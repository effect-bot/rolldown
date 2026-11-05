import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `getBuiltinModule` with a constant specifier naming a builtin that cannot
// run code (Effect probes `node:async_hooks` this way): not a code loader.
const Store = Context.Service('test/RealBuiltinProbe');
const asyncHooks = globalThis.process?.getBuiltinModule?.('node:async_hooks');
const probeUnusedHelper = () => 'probeUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const probeUnused = () => probeUnusedHelper();
    return Store.of({ readOne, probeUnused });
  }),
);

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), typeof asyncHooks?.AsyncLocalStorage];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  'function',
]);
