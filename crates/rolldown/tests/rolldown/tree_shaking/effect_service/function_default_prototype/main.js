import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `Box.prototype` exists before anything assigns it, so a method added to it
// is inherited by every `new Box(...)`.
function Box(value) {
  this.value = value;
}
Box.prototype.getValue = function () {
  return this.value;
};

const Store = Context.Service('test/FunctionDefaultPrototypeStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ prototypeUsed: () => 'prototype', unusedPrototype: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return new Box(store).getValue().prototypeUsed();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'prototype');
