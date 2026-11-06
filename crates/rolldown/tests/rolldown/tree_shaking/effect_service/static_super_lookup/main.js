import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Static `super` refers to `Base`, and `this` inside `Base.get` is `Derived`,
// which holds the service in a static field.
class Base {
  static get() {
    return this.value;
  }
}
class Derived extends Base {
  static read() {
    return super.get();
  }
  static readComputed() {
    return super['get']?.();
  }
}

const Store = Context.Service('test/StaticSuperLookupStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    staticSuperUsed: () => 'static',
    staticSuperComputedUsed: () => 'computed',
    unusedStaticSuper: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  Derived.value = yield* Store;
  return [Derived.read().staticSuperUsed(), Derived.readComputed().staticSuperComputedUsed()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'static',
  'computed',
]);
