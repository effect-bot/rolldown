import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Every function's default prototype, and every class's prototype, has a
// `constructor` property pointing back at it. Each constructor here is reached
// only through that property, from the prototype or from an instance.
function Box(value) {
  this.value = value;
}
class Cell {
  constructor(value) {
    this.value = value;
  }
}
const FromFunctionPrototype = Box.prototype.constructor;
const FromFunctionInstance = new Box(null).constructor;
const FromClassPrototype = Cell.prototype.constructor;
const FromClassInstance = new Cell(null).constructor;

const Store = Context.Service('test/PrototypeConstructorStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    functionPrototypeUsed: () => 'function prototype',
    functionInstanceUsed: () => 'function instance',
    classPrototypeUsed: () => 'class prototype',
    classInstanceUsed: () => 'class instance',
    unusedConstructorReference: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    new FromFunctionPrototype(store).value.functionPrototypeUsed(),
    new FromFunctionInstance(store).value.functionInstanceUsed(),
    new FromClassPrototype(store).value.classPrototypeUsed(),
    new FromClassInstance(store).value.classInstanceUsed(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'function prototype',
  'function instance',
  'class prototype',
  'class instance',
]);
