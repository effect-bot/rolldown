import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Data from 'effect/Data';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A derived class without its own constructor passes its arguments to the base
// constructor, which writes them through `this`; `Data.Class` is used that way.
class Base {
  constructor(props) {
    this.value = props.value;
  }
}
class Derived extends Base {}
class Entry extends Data.Class {}

const Store = Context.Service('test/ImplicitConstructorStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ derivedUsed: () => 'derived', dataUsed: () => 'data', unusedDerived: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    new Derived({ value: store }).value.derivedUsed(),
    new Entry({ value: store }).value.dataUsed(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'derived',
  'data',
]);
