import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Code compiled from text built at run time receives one service, so that
// service keeps every method. The other services are never handed to it:
// `Ambient` is provided alongside it and `Other` elsewhere, and both lose
// their unread methods.
const Store = Context.Service('test/RealOpaqueLoaderStore');
const Ambient = Context.Service('test/RealOpaqueLoaderAmbient');
const Other = Context.Service('test/RealOpaqueLoaderOther');
const loaderStoreUnusedHelper = () => 's2';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readStore = () => 's1';
    const loaderStoreUnused = () => loaderStoreUnusedHelper();
    return Store.of({ readStore, loaderStoreUnused });
  }),
);
const loaderAmbientUnusedHelper = () => 'm2';
const AmbientLayer = Layer.effect(
  Ambient,
  Effect.gen(function* () {
    const readAmbient = () => 'm1';
    const loaderAmbientUnused = () => loaderAmbientUnusedHelper();
    return Ambient.of({ readAmbient, loaderAmbientUnused });
  }),
);
const loaderOtherUnusedHelper = () => 'o2';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'o1';
    const loaderOtherUnused = () => loaderOtherUnusedHelper();
    return Other.of({ readOther, loaderOtherUnused });
  }),
);

const source = ['return store.loaderStore', 'Unused()'].join('');
const runA = Effect.gen(function* () {
  const store = yield* Store;
  const ambient = yield* Ambient;
  return [store.readStore(), ambient.readAmbient(), Function('store', source)(store)];
});
const runB = Effect.gen(function* () {
  return (yield* Other).readOther();
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(runA, Layer.mergeAll(StoreLayer, AmbientLayer))),
  ['s1', 'm1', 's2'],
);
assert.strictEqual(await Effect.runPromise(Effect.provide(runB, OtherLayer)), 'o1');
