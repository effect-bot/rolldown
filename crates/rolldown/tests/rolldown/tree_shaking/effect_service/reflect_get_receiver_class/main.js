import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Reflect.get`'s third argument is the receiver: the getter on
// `Holder.prototype` runs with the plain object as `this`, which does not
// inherit from `Holder`.
class Holder {
  get current() {
    return this.value;
  }
}

const Store = Context.Service('test/ReflectGetReceiverStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ reflectReceiverUsed: () => 'used', reflectReceiverUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return Reflect.get(Holder.prototype, 'current', { value: store }).reflectReceiverUsed();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'used');
