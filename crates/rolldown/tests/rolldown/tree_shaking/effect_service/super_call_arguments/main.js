import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The derived constructor takes the service from its own argument object and
// passes it to the base constructor with `super(...)`. The `new` call never
// passes the service directly, so only `super`'s own arguments carry it.
class Base {
  constructor(value) {
    this.value = value;
  }
}
class Derived extends Base {
  constructor(options) {
    super(options.store);
  }
}
class Fixed extends Base {
  constructor(store) {
    super(store);
    this.kind = 'fixed';
  }
}

const Store = Context.Service('test/SuperCallArgumentsStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    superUsed: () => 'super',
    superFixedUsed: () => 'fixed',
    unusedSuper: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [new Derived({ store }).value.superUsed(), new Fixed(store).value.superFixedUsed()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'super',
  'fixed',
]);
