import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Context.getOrUndefined` with a key chosen at run time from two declared
// keys: a finite set. The static read of `unresolvedOtherUnused` applies to
// both, so Other keeps it, while Store's method of another name goes.
const Store = Context.Service('test/RealLookupUnresolvedStore');
const Other = Context.Service('test/RealLookupUnresolvedOther');

const unresolvedStoreUnusedHelper = () => 'storeUnused';
const unresolvedOtherUnusedHelper = () => 'otherUnused';

const layer = Layer.mergeAll(
  Layer.succeed(Store, {
    readOne: () => 'read',
    unresolvedStoreUnused: () => unresolvedStoreUnusedHelper(),
  }),
  Layer.succeed(Other, {
    readOther: () => 'other',
    unresolvedOtherUnused: () => unresolvedOtherUnusedHelper(),
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const other = yield* Other;
  const key = Number('1') === 1 ? Other : Store;
  const context = yield* Effect.context();
  return [
    store.readOne(),
    other.readOther(),
    Context.getOrUndefined(context, key).unresolvedOtherUnused(),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'read',
  'other',
  'otherUnused',
]);
