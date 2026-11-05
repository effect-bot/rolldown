import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A spread moves the arguments of a keyed lookup: at run time the key is a
// hand-made object with a constant `key` string naming Store, and the trailing
// `Other` is ignored. The spread resolves statically, so the read of
// `spreadStoreRead` applies to Store and Other's unread method goes.
const Store = Context.Service('test/RealLookupSpreadStore');
const Other = Context.Service('test/RealLookupSpreadOther');

const spreadOtherUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    return Store.of({ readOne: () => 'read', spreadStoreRead: () => 'spread' });
  }),
);
const OtherLayer = Layer.succeed(Other, {
  readOther: () => 'other',
  spreadOtherUnused: () => spreadOtherUnusedHelper(),
});

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  return [
    (yield* Other).readOther(),
    Context.get(...[context, { key: 'test/RealLookupSpreadStore' }], Other).spreadStoreRead(),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.merge(StoreLayer, OtherLayer))),
  ['other', 'spread'],
);
