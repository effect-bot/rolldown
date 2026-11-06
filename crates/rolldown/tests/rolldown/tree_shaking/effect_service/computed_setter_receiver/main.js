import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Both setters have computed names the analysis cannot fold to a literal: a
// string returned by a function, and a symbol. Each sits on its own class and
// is written through its own helper with a computed member, on an instance of
// a derived class, so neither case can stand in for the other.
const stringName = (() => 'content')();
const symbolName = Symbol('content');
class StringBase {
  set [stringName](value) {
    this.inner = value;
  }
}
class StringDerived extends StringBase {}
class SymbolBase {
  set [symbolName](value) {
    this.symbolInner = value;
  }
}
class SymbolDerived extends SymbolBase {}
const writeString = (target, value) => {
  target[stringName] = value;
  return target;
};
const writeSymbol = (target, value) => {
  target[symbolName] = value;
  return target;
};

const Store = Context.Service('test/ComputedSetterReceiverStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    computedSetterUsed: () => 'string',
    symbolSetterUsed: () => 'symbol',
    unusedComputedSetter: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    writeString(new StringDerived(), store).inner.computedSetterUsed(),
    writeSymbol(new SymbolDerived(), store).symbolInner.symbolSetterUsed(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'string',
  'symbol',
]);
