import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A subclass of a class-form key is not itself a key: it is ordinary class
// code. Its constructor stores the service in an instance field. Nothing reads
// a method of the service, but it reaches class code, so its key bails and
// every method stays.
class OtherKey extends Context.Service()('test/KeySubclassOther') {}
class Holder extends OtherKey {
  constructor(value) {
    super();
    this.value = value;
  }
}

const Store = Context.Service('test/KeySubclassInstanceFieldStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ subclassFirst: () => 'first', subclassSecond: () => 'second' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return new Holder(store).value === store;
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), true);
