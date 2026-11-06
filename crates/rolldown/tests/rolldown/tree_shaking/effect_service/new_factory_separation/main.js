import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Equal from 'effect/Equal';
import * as Layer from 'effect/Layer';

// One factory calls `new` for both of its callers: the service at one call site,
// plain data at the other.
class Entry {
  constructor(value) {
    this.value = value;
  }
}
const make = (value) => new Entry(value);

const Store = Context.Service('test/NewFactoryStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ newFactoryUsed: () => 'used', unusedNewFactory: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const held = make(yield* Store);
  const plain = make({ n: 1 });
  // Structural equality reads every property of the plain value only.
  return [held.value.newFactoryUsed(), Equal.equals(plain.value, { n: 1 })];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'used',
  true,
]);
