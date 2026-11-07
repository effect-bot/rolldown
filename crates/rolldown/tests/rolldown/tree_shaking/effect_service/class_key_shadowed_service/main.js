import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class extends `Fake.Service()("id")`: spelled like a key constructor, but
// `Fake` is a plain object, not effect's Context. The class is ordinary class
// code, and its method reads the service it closes over: Store keeps every
// method. Other is never mentioned by a class and still prunes.
const Fake = { Service: () => () => class {} };
const Store = Context.Service('test/ClassKeyShadowedServiceStore');
const Other = Context.Service('test/ClassKeyShadowedServiceOther');

const program = Effect.gen(function* () {
  const store = yield* Store;
  class Runner extends Fake.Service()('test/NotAKey') {
    run() {
      return store.shadowedFirst();
    }
  }
  const other = yield* Other;
  return [new Runner().run(), other.shadowedOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, Store.of({ shadowedFirst: () => 'first', shadowedSecond: () => 'second' })),
  Layer.succeed(
    Other,
    Other.of({ shadowedOtherUsed: () => 'other', shadowedOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['first', 'other']);
