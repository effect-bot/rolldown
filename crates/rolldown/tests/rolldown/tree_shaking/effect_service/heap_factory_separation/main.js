import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Equal from 'effect/Equal';
import * as Layer from 'effect/Layer';

// A small factory is cloned per call site, but every clone allocates the same
// abstract `{ value }` unless allocations are tagged with the clone's context.
const wrap = (value) => ({ value });

const Store = Context.Service('test/HeapFactoryStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ heapUsed: () => 'used', unusedHeap: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const held = wrap(yield* Store);
  const plain = wrap({ n: 1 });
  // Structural equality reads every property of the plain value only.
  return [held.value.heapUsed(), Equal.equals(plain.value, { n: 1 })];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'used',
  true,
]);
