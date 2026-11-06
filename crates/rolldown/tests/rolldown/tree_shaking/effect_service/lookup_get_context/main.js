import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/RealLookupgetStore');
const lookupGetUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const lookupGetUnused = () => lookupGetUnusedHelper();
    return Store.of({ readOne, lookupGetUnused });
  }),
);
const name = ['lookupGet', 'Unused'].join('');
const Box = Context.Service('test/RealLookupGetBox');
const BoxLayer = Layer.effect(
  Box,
  Effect.map(Effect.context(), (context) => Box.of({ context })),
);
// `Context.get` of a key whose service holds the Context.
const AppLayer = Layer.provideMerge(BoxLayer, StoreLayer);
const program = Effect.gen(function* () {
  const box = Context.get(yield* Effect.context(), Box);
  const out = [];
  out.push(Context.get(box.context, Store)[name]());
  return [(yield* Store).readOne(), out];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), ['r', ['u']]);
