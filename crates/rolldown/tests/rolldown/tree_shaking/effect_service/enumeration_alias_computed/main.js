import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: a service read out of the Context with `Context.get` and called by
// a name built at run time may use any method. Every method stays.
const Store = Context.Service('test/RealEnumerationComputed');
const enumComputedUnusedHelper = () => 'enumComputedUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const enumComputedUnused = () => enumComputedUnusedHelper();
    return Store.of({ readOne, enumComputedUnused });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const context = yield* Effect.context();
  const name = ['enumComputed', 'Unused'].join('');
  const results = [];
  results.push(Context.get(context, Store)[name]());
  return [store.readOne(), results];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  ['enumComputedUnused'],
]);
