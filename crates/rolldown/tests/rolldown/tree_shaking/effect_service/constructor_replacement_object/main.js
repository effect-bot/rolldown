import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A constructor that returns an object makes that object the result of `new`.
// For a derived class, the object the parent constructor returns also becomes
// `this` in the derived constructor after `super(...)`.
class ClassBox {
  constructor(value) {
    return value;
  }
}
function FunctionBase(value) {
  return value;
}
class FromFunction extends FunctionBase {}
class ClassBase {
  constructor(value) {
    return value;
  }
}
class FromClass extends ClassBase {}
class ThisAfterSuper extends ClassBase {
  constructor(value) {
    super(value);
    this.seen = this.replacementThisUsed();
  }
}

const Store = Context.Service('test/ConstructorReplacementStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    replacementClassUsed: () => 'class',
    replacementFunctionBaseUsed: () => 'function base',
    replacementClassBaseUsed: () => 'class base',
    replacementThisUsed: () => 'this',
    unusedReplacement: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const afterSuper = new ThisAfterSuper(store);
  return [
    new ClassBox(store).replacementClassUsed(),
    new FromFunction(store).replacementFunctionBaseUsed(),
    new FromClass(store).replacementClassBaseUsed(),
    afterSuper === store ? afterSuper.seen : 'not replaced',
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'class',
  'function base',
  'class base',
  'this',
]);
