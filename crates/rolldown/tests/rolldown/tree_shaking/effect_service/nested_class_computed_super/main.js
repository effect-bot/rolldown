import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The key expression `[super.get().nestedSuperUsed()]` runs inside
// `Derived.read`, where `super` is `Base` and `this` is `Derived`; `Inner`'s own
// parent, `Unrelated`, plays no part in it.
class Base {
  static get() {
    return this.value;
  }
}
class Unrelated {}
class Derived extends Base {
  static read(value) {
    this.value = value;
    class Inner extends Unrelated {
      [super.get().nestedSuperUsed()]() {
        return 'nested';
      }
    }
    return new Inner().computed();
  }
}

const Store = Context.Service('test/NestedClassComputedSuperStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ nestedSuperUsed: () => 'computed', unusedNestedSuper: () => 'unused' }),
);

const program = Effect.gen(function* () {
  return Derived.read(yield* Store);
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'nested');
