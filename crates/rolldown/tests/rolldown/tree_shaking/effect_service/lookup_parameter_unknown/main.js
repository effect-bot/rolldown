import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The lookup's key argument is one of two declared keys, chosen at run time
// and passed through a parameter. The read is static, so it applies to both
// keys: `unknownOtherUnused` stays on Other, and Store's method of another
// name, which nothing reads, goes.
const Store = Context.Service('test/RealParamUnknownStore');
const Other = Context.Service('test/RealParamUnknownOther');
const lookupIn = (context, key) => Context.get(context, key);
const unknownStoreUnusedHelper = () => 'unknownStoreUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const unknownStoreUnused = () => unknownStoreUnusedHelper();
    return Store.of({ readOne, unknownStoreUnused });
  }),
);
const unknownOtherUnusedHelper = () => 'unknownOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const unknownOtherUnused = () => unknownOtherUnusedHelper();
    return Other.of({ readOther, unknownOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  const key = Number('1') === 1 ? Other : Store;
  return [(yield* Store).readOne(), lookupIn(context, key).unknownOtherUnused()];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readOne', 'unknownOtherUnused'],
);
