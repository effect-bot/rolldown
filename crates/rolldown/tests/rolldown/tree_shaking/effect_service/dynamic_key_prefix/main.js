import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A key built at run time with a constant prefix can only alias tracked keys
// with that prefix: those keep every method, other keys can still be pruned.
const PrefixStore = Context.Service('test/RealPrefix/A');
const OtherStore = Context.Service('test/RealOther/B');
const suffix = ['A'].join('');
const Dynamic = Context.Service(`test/RealPrefix/${suffix}`);
const prefixUnusedHelper = () => 'prefixUnused';
const PrefixLayer = Layer.effect(
  PrefixStore,
  Effect.gen(function* () {
    const readPrefix = () => 'readPrefix';
    const prefixUnused = () => prefixUnusedHelper();
    return PrefixStore.of({ readPrefix, prefixUnused });
  }),
);
const otherPrefixUnusedHelper = () => 'otherPrefixUnused';
const OtherLayer = Layer.effect(
  OtherStore,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const otherPrefixUnused = () => otherPrefixUnusedHelper();
    return OtherStore.of({ readOther, otherPrefixUnused });
  }),
);

const program = Effect.gen(function* () {
  return [
    (yield* PrefixStore).readPrefix(),
    (yield* OtherStore).readOther(),
    (yield* Dynamic).prefixUnused(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(PrefixLayer, OtherLayer))),
  ['readPrefix', 'readOther', 'prefixUnused'],
);
