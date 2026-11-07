import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class method looks the service up by its key string with
// Context.getOrUndefinedUnsafe. The class body can reach any key that way, so
// Store keeps every method.
const Store = Context.Service('test/ClassHoleUnsafeLookupStore');
class Lookup {
  constructor(context) {
    this.context = context;
  }
  read() {
    return Context.getOrUndefinedUnsafe(
      this.context,
      'test/ClassHoleUnsafeLookupStore',
    ).unsafeUsed();
  }
}

const program = Effect.gen(function* () {
  yield* Store;
  const context = yield* Effect.context();
  return new Lookup(context).read();
});

const layer = Layer.succeed(
  Store,
  Store.of({ unsafeUsed: () => 'used', unsafeUnread: () => 'unread' }),
);
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
