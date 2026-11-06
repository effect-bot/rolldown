import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { readRegistered, register } from './late.js';

// Code the pass cannot see reaches a service through ordinary values it is
// given. Store is handed over inside a plain object, which the external module
// keeps and later reads a method of by a name built at run time, so Store's key
// bails. Other is never handed over and loses its unread method.
const Store = Context.Service('test/OpaqueWrappedStore');
const Other = Context.Service('test/OpaqueWrappedOther');

const wrappedStoreUnusedHelper = () => 's2';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readStore: () => 's1', wrappedStoreUnused: () => wrappedStoreUnusedHelper() }),
);
const wrappedOtherUnusedHelper = () => 'o2';
const OtherLayer = Layer.succeed(
  Other,
  Other.of({ readOther: () => 'o1', wrappedOtherUnused: () => wrappedOtherUnusedHelper() }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const other = yield* Other;
  register({ store });
  return [store.readStore(), other.readOther(), readRegistered()];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['s1', 'o1', 's2'],
);
