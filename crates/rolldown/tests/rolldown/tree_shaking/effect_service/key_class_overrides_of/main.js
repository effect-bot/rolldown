import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class-form key that overrides the inherited `of`: `Store.of(value)` is no
// longer the identity, and here it reads a method of its argument that no
// consumer reads directly.
class Store extends Context.Service()('test/RealKeyClassOverridesOf') {
  static of(value) {
    return { readOne: () => value.overridesOfUnused() };
  }
}

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const overridesOfUnusedHelper = () => 'unused';
    return Store.of({ readOne: () => 'read', overridesOfUnused: () => overridesOfUnusedHelper() });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.readOne();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'unused');
