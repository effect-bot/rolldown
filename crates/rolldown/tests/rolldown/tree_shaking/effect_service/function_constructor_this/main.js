import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';

// `new` on a function constructor returns the object its body writes through
// `this`; `Option.some` is built that way. Its service is a separate one: a
// value handed to `Option.some` is a record at a type parameter's position of
// type-modelled code, so its members all count as read (design.md, options
// object rule), and only its read method is checked.
function Box(value) {
  this.value = value;
}

const Store = Context.Service('test/FunctionConstructorStore');
const SomeStore = Context.Service('test/FunctionConstructorSome');
const StoreLayer = Layer.mergeAll(
  Layer.succeed(Store, Store.of({ boxUsed: () => 'box', unusedBox: () => 'unused' })),
  Layer.succeed(SomeStore, SomeStore.of({ someUsed: () => 'some', someUnused: () => 'unused' })),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const someStore = yield* SomeStore;
  return [new Box(store).value.boxUsed(), Option.getOrThrow(Option.some(someStore)).someUsed()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'box',
  'some',
]);
