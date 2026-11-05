import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { readWith } from './late.js';

// Run A hands its service to an external module that does not exist when the
// program is bundled, so nothing is known about what it reads: run A's service
// keeps every method. Run B never calls it and shares nothing with run A, so
// its unread method can go.
const Store = Context.Service('test/RealExternalUnresolvableStore');
const Other = Context.Service('test/RealExternalUnresolvableOther');
const lateStoreUnusedHelper = () => 's2';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readStore = () => 's1';
    const lateStoreUnused = () => lateStoreUnusedHelper();
    return Store.of({ readStore, lateStoreUnused });
  }),
);
const lateOtherUnusedHelper = () => 'o2';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'o1';
    const lateOtherUnused = () => lateOtherUnusedHelper();
    return Other.of({ readOther, lateOtherUnused });
  }),
);

const runA = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readStore(), readWith(store)];
});
const runB = Effect.gen(function* () {
  return (yield* Other).readOther();
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(runA, StoreLayer)), ['s1', 's2']);
assert.strictEqual(await Effect.runPromise(Effect.provide(runB, OtherLayer)), 'o1');
