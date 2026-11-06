import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `super.get()` finds `get` on `Base.prototype`, but `get` runs with the
// derived instance as `this`, so `this.value` is the service.
class Base {
  constructor(value) {
    this.value = value;
  }
  get() {
    return this.value;
  }
}
class Derived extends Base {
  constructor(value) {
    super(value);
  }
  read() {
    return super.get();
  }
}

const Store = Context.Service('test/SuperMethodReceiverStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ superMethodUsed: () => 'super method', unusedSuperMethod: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return new Derived(store).read().superMethodUsed();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'super method');
