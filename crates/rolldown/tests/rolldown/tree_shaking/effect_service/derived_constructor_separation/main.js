import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Equal from 'effect/Equal';
import * as Layer from 'effect/Layer';

// A derived class without its own constructor runs the base constructor, which
// writes `this.value` for instances built at two `new` sites.
class Base {
  constructor(value) {
    this.value = value;
  }
}
class Entry extends Base {}

const Store = Context.Service('test/DerivedSeparationStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ derivedSeparationUsed: () => 'used', unusedDerivedSeparation: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const held = new Entry(yield* Store);
  const plain = new Entry({ n: 1 });
  // Structural equality reads every property of the plain value only.
  return [held.value.derivedSeparationUsed(), Equal.equals(plain.value, { n: 1 })];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'used',
  true,
]);
