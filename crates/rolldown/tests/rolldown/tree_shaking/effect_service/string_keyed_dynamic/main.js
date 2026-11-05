import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: the key string is read from the environment at run time (a folded
// constant such as `["a", "b"].join("/")` would be precise), so any service
// may be read. Every method of both stays.
const Store = Context.Service('test/RealStringDynamicStore');
const Other = Context.Service('test/RealStringDynamicOther');
const stringDynamicStoreUnusedHelper = () => 'stringDynamicStoreUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const stringDynamicStoreUnused = () => stringDynamicStoreUnusedHelper();
    return Store.of({ readOne, stringDynamicStoreUnused });
  }),
);
const stringDynamicOtherUnusedHelper = () => 'stringDynamicOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const stringDynamicOtherUnused = () => stringDynamicOtherUnusedHelper();
    return Other.of({ readOther, stringDynamicOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  const id = String(process.env.EFFECT_FIXTURE_KEY ?? 'test/RealStringDynamicOther');
  return [
    (yield* Store).readOne(),
    Context.getOrUndefinedUnsafe(context, id).stringDynamicOtherUnused(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readOne', 'stringDynamicOtherUnused'],
);
