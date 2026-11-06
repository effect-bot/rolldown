import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: the key string is read from the environment at run time (a folded
// constant such as `["a", "b"].join("/")` would be precise), so the lookup may
// read any service. Both services have a method with the name read through it,
// so that method stays on both. The property name is known, so methods nothing
// reads are still removed.
const Store = Context.Service('test/RealStringDynamicStore');
const Other = Context.Service('test/RealStringDynamicOther');
const stringDynamicStoreSharedHelper = () => 'sharedStore';
const stringDynamicStoreUnusedHelper = () => 'stringDynamicStoreUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const stringDynamicShared = () => stringDynamicStoreSharedHelper();
    const stringDynamicStoreUnused = () => stringDynamicStoreUnusedHelper();
    return Store.of({ readOne, stringDynamicShared, stringDynamicStoreUnused });
  }),
);
const stringDynamicOtherSharedHelper = () => 'sharedOther';
const stringDynamicOtherUnusedHelper = () => 'stringDynamicOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const stringDynamicShared = () => stringDynamicOtherSharedHelper();
    const stringDynamicOtherUnused = () => stringDynamicOtherUnusedHelper();
    return Other.of({ readOther, stringDynamicShared, stringDynamicOtherUnused });
  }),
);

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  const id = String(process.env.EFFECT_FIXTURE_KEY ?? 'test/RealStringDynamicOther');
  return [
    (yield* Store).readOne(),
    Context.getOrUndefinedUnsafe(context, id).stringDynamicShared(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['readOne', 'sharedOther'],
);
