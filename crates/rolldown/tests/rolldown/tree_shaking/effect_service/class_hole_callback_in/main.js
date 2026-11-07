import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A closure that reads the service is handed to a class constructor, and a
// method calls it later. The service never enters the class, only the
// closure, which still runs: the method it reads stays, the unread one goes,
// and Store does not bail.
const Store = Context.Service('test/ClassHoleCallbackInStore');
class Deferred {
  constructor(callback) {
    this.callback = callback;
  }
  fire() {
    return this.callback();
  }
}

const program = Effect.gen(function* () {
  const store = yield* Store;
  return new Deferred(() => store.callbackUsed()).fire();
});

const layer = Layer.succeed(
  Store,
  Store.of({ callbackUsed: () => 'used', callbackUnused: () => 'unused' }),
);
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
