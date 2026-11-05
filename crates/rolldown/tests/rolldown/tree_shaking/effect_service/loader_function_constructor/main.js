import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The Function constructor, reached through a computed name on an ordinary
// function and on a global builtin, compiles code outside the bundle. That code
// receives no value and no Effect primitive, and its result is not run as an
// Effect, so nothing escapes and the unread method is removed.
const Store = Context.Service('test/RealLoaderFunctionConstructor');

const functionCtorUnusedHelper = () => 'unused';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'read';
    const functionCtorUnused = () => functionCtorUnusedHelper();
    return Store.of({ readOne, functionCtorUnused });
  }),
);

const loadFromArrow = (() => 0)['con' + 'structor'];
const loadFromGlobal = globalThis.Object['con' + 'structor'];

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [store.readOne(), loadFromArrow('return 1')(), loadFromGlobal('return 2')()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'read',
  1,
  2,
]);
