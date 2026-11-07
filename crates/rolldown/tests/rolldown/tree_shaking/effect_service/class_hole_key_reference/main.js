import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class method yields the key itself: nothing is handed to the class, but
// the class body fetches the service, so its key keeps every method. Other is
// never mentioned by a class and still prunes.
const Store = Context.Service('test/ClassHoleKeyReferenceStore');
const Other = Context.Service('test/ClassHoleKeyReferenceOther');
class Runner {
  run() {
    return Effect.gen(function* () {
      const store = yield* Store;
      return store.keyRefUsed();
    });
  }
}

const program = Effect.gen(function* () {
  const fromClass = yield* new Runner().run();
  const other = yield* Other;
  return [fromClass, other.keyRefOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, Store.of({ keyRefUsed: () => 'used', keyRefUnread: () => 'unread' })),
  Layer.succeed(
    Other,
    Other.of({
      keyRefOtherUsed: () => 'other',
      keyRefOtherUnused: () => 'unused',
    }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'other']);
