import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// An Effect's iterator is a member no rule names; following it reaches the
// Effect's success value, the service, whose method is then called.
const Store = Context.Service('test/UnruledMemberStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ unruledUsed: () => 'used', unusedUnruled: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const effect = Effect.succeed(yield* Store);
  return effect[Symbol.iterator]().next().value.value.unruledUsed();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'used');
