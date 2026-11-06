import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';

// `new` on a function constructor returns the object its body writes through
// `this`; `Option.some` is built that way.
function Box(value) {
  this.value = value;
}

const Store = Context.Service('test/FunctionConstructorStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ boxUsed: () => 'box', someUsed: () => 'some', unusedBox: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [new Box(store).value.boxUsed(), Option.getOrThrow(Option.some(store)).someUsed()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'box',
  'some',
]);
