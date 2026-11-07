import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The getter is an ordinary function installed on a plain object, not class
// code: it reads the service through `this`, the object it is accessed on.
const Store = Context.Service('test/DefinePropertyGetterStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ definePropertyUsed: () => 'used', definePropertyUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const holder = { inner: yield* Store };
  Object.defineProperty(holder, 'store', {
    get() {
      return this.inner;
    },
  });
  return holder.store.definePropertyUsed();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'used');
