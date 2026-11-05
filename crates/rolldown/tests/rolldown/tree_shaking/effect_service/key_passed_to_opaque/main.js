import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A key flows into a function chosen by an index computed at run time from a
// constant array. Every element is a closure the pass can read, and the one here
// returns its argument, so the key never reaches opaque code: both keys prune.
const Store = Context.Service('test/RealKeyOpaqueStore');
const Other = Context.Service('test/RealKeyOpaqueOther');
const sinks = [(key) => key];
const opaqueStoreUnusedHelper = () => 'opaqueStoreUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const opaqueStoreUnused = () => opaqueStoreUnusedHelper();
    return Store.of({ readOne, opaqueStoreUnused });
  }),
);
const opaqueOtherUnusedHelper = () => 'opaqueOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const opaqueOtherUnused = () => opaqueOtherUnusedHelper();
    return Other.of({ readOther, opaqueOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  const leaked = sinks[Number('0')](Store);
  return [(yield* Store).readOne(), (yield* Other).readOther(), leaked === Store];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readOne', 'readOther', true],
);
