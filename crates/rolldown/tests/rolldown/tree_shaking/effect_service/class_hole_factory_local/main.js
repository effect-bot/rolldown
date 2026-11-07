import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A factory returns a class whose method reads the factory's parameter. The
// parameter is a binding outside the class body that the body references, so
// the service in it reaches class code: Store keeps every method. Other still
// prunes.
const Store = Context.Service('test/ClassHoleFactoryLocalStore');
const Other = Context.Service('test/ClassHoleFactoryLocalOther');
const make = (store) =>
  class {
    run() {
      return store.factoryUsed();
    }
  };

const program = Effect.gen(function* () {
  const Made = make(yield* Store);
  const other = yield* Other;
  return [new Made().run(), other.factoryOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, Store.of({ factoryUsed: () => 'used', factoryUnread: () => 'unread' })),
  Layer.succeed(
    Other,
    Other.of({
      factoryOtherUsed: () => 'other',
      factoryOtherUnused: () => 'unused',
    }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'other']);
