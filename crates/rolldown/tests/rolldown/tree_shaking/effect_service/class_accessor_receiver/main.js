import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Each accessor reads or writes the instance's own fields through `this`. The
// getters' results are called with `?.()`, so a getter that ran with the wrong
// `this` would skip the call silently: the program checks every result.
class Base {
  constructor(value) {
    this.value = value;
  }
  get handler() {
    return this.value.accessorDirectUsed;
  }
  get superHandler() {
    return this.value.accessorSuperUsed;
  }
}
class Derived extends Base {
  read() {
    return super['superHandler']?.();
  }
}
class Box {
  set content(value) {
    this.inner = value;
  }
}

const Store = Context.Service('test/ClassAccessorReceiverStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    accessorDirectUsed: () => 'getter',
    accessorSuperUsed: () => 'super getter',
    accessorSetterUsed: () => 'setter',
    unusedAccessor: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const box = new Box();
  box.content = store;
  return [new Base(store).handler?.(), new Derived(store).read(), box.inner.accessorSetterUsed()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'getter',
  'super getter',
  'setter',
]);
