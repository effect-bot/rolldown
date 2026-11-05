import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class-form key whose static method reads the service through `this`. The
// read is analysed like any other: the method it names stays.
class Store extends Context.Service()('test/RealKeyClassStaticMember') {
  static readUnused() {
    return Effect.map(this, (store) => store.staticMemberUnused());
  }
}

const staticMemberUnusedHelper = () => 'unused';

const StoreLayer = Layer.succeed(Store, {
  readOne: () => 'read',
  staticMemberUnused: () => staticMemberUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), yield* Store.readUnused()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  'unused',
]);
