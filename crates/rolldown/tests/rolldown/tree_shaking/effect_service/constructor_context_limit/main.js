import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// One constructor runs at 65 `new` sites, one more than the per-constructor
// context limit; the last instance holds the service.
class Entry {
  constructor(value) {
    this.value = value;
  }
}
const plain = { constructorLimitUsed: () => 'plain' };

const Store = Context.Service('test/ConstructorLimitStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ constructorLimitUsed: () => 'used', unusedConstructorLimit: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const entries = [
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(plain),
    new Entry(store),
  ];
  return entries.map((entry) => entry.value.constructorLimitUsed());
});
const out = await Effect.runPromise(Effect.provide(program, StoreLayer));
assert.strictEqual(out.length, 65);
assert.strictEqual(out[64], 'used');
