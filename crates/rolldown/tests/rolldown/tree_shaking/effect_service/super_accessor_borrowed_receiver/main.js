import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `read` and `write` look `content` up on their parent prototype through
// `super`, but run with a plain object as `this`, which does not inherit from
// either class. The getter and the setter live on separate classes, so neither
// case can stand in for the other.
class GetterBase {
  get content() {
    return this.value;
  }
}
class GetterDerived extends GetterBase {
  read() {
    return super.content;
  }
}
class SetterBase {
  set content(value) {
    this.inner = value;
  }
}
class SetterDerived extends SetterBase {
  write(value) {
    super.content = value;
    return this.inner;
  }
}
const readOn = (method, receiver) => method.call(receiver);
const writeOn = (method, receiver, value) => method.call(receiver, value);

const Store = Context.Service('test/SuperAccessorBorrowedStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    borrowedGetterUsed: () => 'getter',
    borrowedSetterUsed: () => 'setter',
    unusedBorrowed: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    readOn(GetterDerived.prototype.read, { value: store }).borrowedGetterUsed(),
    writeOn(SetterDerived.prototype.write, {}, store).borrowedSetterUsed(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'getter',
  'setter',
]);
