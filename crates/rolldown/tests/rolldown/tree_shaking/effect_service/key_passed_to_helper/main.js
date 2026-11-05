import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A key passed to a function whose body only reads its key string. The code
// shows it cannot change or leak the key, so other keys can still be pruned.
const Store = Context.Service('test/RealKeyHelperStore');
const Other = Context.Service('test/RealKeyHelperOther');
const keyName = (key) => key.key;
const helperStoreUnusedHelper = () => 'helperStoreUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const helperStoreUnused = () => helperStoreUnusedHelper();
    return Store.of({ readOne, helperStoreUnused });
  }),
);
const helperOtherUnusedHelper = () => 'helperOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const helperOtherUnused = () => helperOtherUnusedHelper();
    return Other.of({ readOther, helperOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), (yield* Other).readOther(), keyName(Store)];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readOne', 'readOther', 'test/RealKeyHelperStore'],
);
