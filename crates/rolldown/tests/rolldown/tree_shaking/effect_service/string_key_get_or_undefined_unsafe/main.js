import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Context.getOrUndefinedUnsafe` with a literal key string reaches exactly the
// service with that key, like a lookup by the key object.
const Store = Context.Service('test/RealGetOrUndefinedUnsafe');
const Other = Context.Service('test/RealGetOrUndefinedUnsafeOther');

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const lookedUpByString = () => 'looked up';
    return Store.of({ readOne, lookedUpByString });
  }),
);
const stringLookupOtherUnusedHelper = () => 'other';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'read other';
    const stringLookupOtherUnused = () => stringLookupOtherUnusedHelper();
    return Other.of({ readOther, stringLookupOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const context = yield* Effect.context();
  return [
    store.readOne(),
    (yield* Other).readOther(),
    Context.getOrUndefinedUnsafe(context, 'test/RealGetOrUndefinedUnsafe').lookedUpByString(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['read', 'read other', 'looked up'],
);
