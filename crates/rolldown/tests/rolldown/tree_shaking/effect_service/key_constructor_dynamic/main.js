import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: the wrapper is called with a string built at run time, so the key
// may alias any service. Every method stays.
const makeKey = (id) => Context.Service(id);
const Store = Context.Service('test/RealWrapperDynamic');
const Dynamic = makeKey(['test', 'RealWrapperDynamic'].join('/'));
const wrapperDynamicUnusedHelper = () => 'wrapperDynamicUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const wrapperDynamicUnused = () => wrapperDynamicUnusedHelper();
    return Store.of({ readOne, wrapperDynamicUnused });
  }),
);

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), (yield* Dynamic).wrapperDynamicUnused()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  'wrapperDynamicUnused',
]);
