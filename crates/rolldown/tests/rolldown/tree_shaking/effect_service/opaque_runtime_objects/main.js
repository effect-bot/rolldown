import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as ManagedRuntime from 'effect/ManagedRuntime';
import { describe, readDirect } from './late.js';

// Code the pass cannot see reaches a service only when it is handed the service
// object itself. Store reaches the external module only through runtime objects
// (its Context, Layer, a ManagedRuntime, an Effect, the running Fiber and the
// current-fiber global), so its unread method goes. Direct is handed over itself,
// and the external module reads a method of it by a name built at run time.
const Store = Context.Service('test/OpaqueRuntimeObjectsStore');
const Direct = Context.Service('test/OpaqueRuntimeObjectsDirect');

const opaqueStoreUnusedHelper = () => 's2';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readStore: () => 's1', opaqueStoreUnused: () => opaqueStoreUnusedHelper() }),
);
const opaqueDirectUnusedHelper = () => 'd2';
const DirectLayer = Layer.succeed(
  Direct,
  Direct.of({ readDirect: () => 'd1', opaqueDirectUnused: () => opaqueDirectUnusedHelper() }),
);

const runtime = ManagedRuntime.make(StoreLayer);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const direct = yield* Direct;
  const context = yield* Effect.context();
  const fiber = yield* Effect.withFiber((fiber) => Effect.succeed(fiber));
  return [
    store.readStore(),
    direct.readDirect(),
    readDirect(direct),
    describe(
      context,
      StoreLayer,
      runtime,
      Effect.succeed(store),
      fiber,
      globalThis['~effect/Fiber/currentFiber'],
      Store,
      Effect.context,
      Context,
    ),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, DirectLayer))),
  ['s1', 'd1', 'd2', 9],
);
assert.strictEqual(
  await runtime.runPromise(
    Effect.gen(function* () {
      return (yield* Store).readStore();
    }),
  ),
  's1',
);
await runtime.dispose();
