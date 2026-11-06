import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Following an Effect's iterator to `.next().value.value` reaches the Effect's
// success value, the service, whose method is then called. `.value` on the
// yielded Effect does not type check against Effect's public types, so the pass
// assumes this does not happen (Tim's decision) and the read keeps nothing.
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
