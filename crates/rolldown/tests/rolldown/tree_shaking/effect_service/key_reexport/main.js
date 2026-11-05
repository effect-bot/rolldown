import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { BillingStore, Other } from './keys.js';

// Keys re-exported by a module that is not an entry, imported and consumed by
// `yield*`. The export specifier only names the binding.
const reexportStoreUnusedHelper = () => 'reexportStoreUnused';
const StoreLayer = Layer.effect(
  BillingStore,
  Effect.gen(function* () {
    const readStore = () => 'readStore';
    const reexportStoreUnused = () => reexportStoreUnusedHelper();
    return BillingStore.of({ readStore, reexportStoreUnused });
  }),
);
const reexportOtherUnusedHelper = () => 'reexportOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const reexportOtherUnused = () => reexportOtherUnusedHelper();
    return Other.of({ readOther, reexportOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  return [(yield* BillingStore).readStore(), (yield* Other).readOther()];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readStore', 'readOther'],
);
