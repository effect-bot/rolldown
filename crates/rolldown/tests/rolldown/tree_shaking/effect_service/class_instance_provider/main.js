import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The service is an instance of a class, not an object literal. Pruning
// prototype methods is a later increment, so this fixture only checks that
// the program behaves the same and keeps what it calls.
const Store = Context.Service('test/ClassInstance');

class FsImpl {
  instanceRead() {
    return 'read';
  }
  instanceUnused() {
    return 'unused';
  }
}

const StoreLayer = Layer.sync(Store, () => new FsImpl());

const program = Effect.gen(function* () {
  return (yield* Store).instanceRead();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read');
