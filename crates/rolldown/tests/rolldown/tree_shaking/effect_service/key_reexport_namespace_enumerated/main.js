import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Lib from './index.js';
import { BillingStore, Other } from './keys.js';

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

// The namespace is enumerated, so every key it exports reaches a consumer
// that reads a method chosen at run time. Every method must be kept.
const program = Effect.gen(function* () {
  const out = [];
  for (const key of Object.values(Lib.Keys)) {
    const service = yield* key;
    const name = Object.keys(service).find((n) => n.endsWith('Unused'));
    out.push(service[name]());
  }
  return out.sort();
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['reexportOtherUnused', 'reexportStoreUnused'],
);
