import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Base` is declared as a key constructor's result, then reassigned to an
// ordinary class before a class extends it. The class is ordinary class code,
// and its method reads the service it closes over: Store keeps every method.
// Other is never mentioned by a class and still prunes.
let Base = Context.Service()('test/ClassKeyReassignedHeritageBase');
Base = class {};
const Store = Context.Service('test/ClassKeyReassignedHeritageStore');
const Other = Context.Service('test/ClassKeyReassignedHeritageOther');

const program = Effect.gen(function* () {
  const store = yield* Store;
  class Runner extends Base {
    run() {
      return store.reassignedFirst();
    }
  }
  const other = yield* Other;
  return [new Runner().run(), other.reassignedOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    Store,
    Store.of({ reassignedFirst: () => 'first', reassignedSecond: () => 'second' }),
  ),
  Layer.succeed(
    Other,
    Other.of({ reassignedOtherUsed: () => 'other', reassignedOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['first', 'other']);
