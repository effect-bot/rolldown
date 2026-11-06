import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A key whose string is a constant prefix plus a suffix read from the
// environment at run time (a folded constant such as `["A"].join("")` would be
// precise). It may be any key with the prefix, so the method read through it
// stays on both prefixed keys, not only on the run-time default. The key
// without the prefix cannot be read through it: its method of the same name is
// removed. The property name is known, so methods nothing reads are removed.
const PrefixStoreA = Context.Service('test/RealPrefix/A');
const PrefixStoreC = Context.Service('test/RealPrefix/C');
const OtherStore = Context.Service('test/RealOther/B');
const suffix = String(process.env.EFFECT_FIXTURE_SUFFIX ?? 'C');
const Dynamic = Context.Service(`test/RealPrefix/${suffix}`);

const prefixSharedHelperA = () => 'sharedA';
const prefixUnusedHelperA = () => 'prefixUnusedA';
const PrefixLayerA = Layer.effect(
  PrefixStoreA,
  Effect.gen(function* () {
    const readPrefixA = () => 'readPrefixA';
    const prefixShared = () => prefixSharedHelperA();
    const prefixUnusedA = () => prefixUnusedHelperA();
    return PrefixStoreA.of({ readPrefixA, prefixShared, prefixUnusedA });
  }),
);
const prefixSharedHelperC = () => 'sharedC';
const prefixUnusedHelperC = () => 'prefixUnusedC';
const PrefixLayerC = Layer.effect(
  PrefixStoreC,
  Effect.gen(function* () {
    const readPrefixC = () => 'readPrefixC';
    const prefixShared = () => prefixSharedHelperC();
    const prefixUnusedC = () => prefixUnusedHelperC();
    return PrefixStoreC.of({ readPrefixC, prefixShared, prefixUnusedC });
  }),
);
const otherPrefixSharedHelper = () => 'sharedOther';
const otherPrefixUnusedHelper = () => 'otherPrefixUnused';
const OtherLayer = Layer.effect(
  OtherStore,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const prefixShared = () => otherPrefixSharedHelper();
    const otherPrefixUnused = () => otherPrefixUnusedHelper();
    return OtherStore.of({ readOther, prefixShared, otherPrefixUnused });
  }),
);

const program = Effect.gen(function* () {
  return [
    (yield* PrefixStoreA).readPrefixA(),
    (yield* PrefixStoreC).readPrefixC(),
    (yield* OtherStore).readOther(),
    (yield* Dynamic).prefixShared(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(program, Layer.mergeAll(PrefixLayerA, PrefixLayerC, OtherLayer)),
  ),
  ['readPrefixA', 'readPrefixC', 'readOther', `shared${suffix}`],
);