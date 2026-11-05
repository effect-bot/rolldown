import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A string-keyed lookup with a constant key string reaches only the service
// with that key. The method it reads stays; other keys can still be pruned.
const Store = Context.Service('test/RealStringKeyedStore');
const Other = Context.Service('test/RealStringKeyedOther');
const stringStoreUnusedHelper = () => 'stringStoreUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const stringStoreUnused = () => stringStoreUnusedHelper();
    return Store.of({ readOne, stringStoreUnused });
  }),
);
const stringOtherUnusedHelper = () => 'stringOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const stringOtherUnused = () => stringOtherUnusedHelper();
    return Other.of({ readOther, stringOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  return [
    (yield* Other).readOther(),
    Context.getOrUndefinedUnsafe(context, 'test/RealStringKeyedStore').stringStoreUnused(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readOther', 'stringStoreUnused'],
);
