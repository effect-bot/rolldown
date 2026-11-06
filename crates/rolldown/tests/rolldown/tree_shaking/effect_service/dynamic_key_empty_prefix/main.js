import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: a key whose string is read from the environment at run time, with
// no constant prefix, may alias any key (a folded constant such as
// `["a", "b"].join("/")` would be precise). Both services have a method with
// the name read through it, so that method stays on both. The property name is
// known, so methods nothing reads are still removed.
const PrefixStore = Context.Service('test/RealEmptyPrefix/A');
const OtherStore = Context.Service('test/RealEmptyPrefix/B');
const id = String(process.env.EFFECT_FIXTURE_KEY ?? 'test/RealEmptyPrefix/B');
const Dynamic = Context.Service(`${id}`);
const emptyPrefixSharedHelperA = () => 'sharedA';
const emptyPrefixUnusedHelperA = () => 'emptyPrefixUnusedA';
const PrefixLayer = Layer.effect(
  PrefixStore,
  Effect.gen(function* () {
    const readPrefix = () => 'readPrefix';
    const emptyPrefixShared = () => emptyPrefixSharedHelperA();
    const emptyPrefixUnusedA = () => emptyPrefixUnusedHelperA();
    return PrefixStore.of({ readPrefix, emptyPrefixShared, emptyPrefixUnusedA });
  }),
);
const emptyPrefixSharedHelperB = () => 'sharedB';
const emptyPrefixUnusedHelperB = () => 'emptyPrefixUnusedB';
const OtherLayer = Layer.effect(
  OtherStore,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const emptyPrefixShared = () => emptyPrefixSharedHelperB();
    const emptyPrefixUnusedB = () => emptyPrefixUnusedHelperB();
    return OtherStore.of({ readOther, emptyPrefixShared, emptyPrefixUnusedB });
  }),
);

const program = Effect.gen(function* () {
  return [
    (yield* PrefixStore).readPrefix(),
    (yield* OtherStore).readOther(),
    (yield* Dynamic).emptyPrefixShared(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(PrefixLayer, OtherLayer))),
  ['readPrefix', 'readOther', 'sharedB'],
);
