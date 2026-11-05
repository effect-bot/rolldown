import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/RealPipedStore');
const pipedUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const pipedUnused = () => pipedUnusedHelper();
    return Store.of({ readOne, pipedUnused });
  }),
);
const name = ['piped', 'Unused'].join('');
// A provider applied through `pipe`: the Effect it is given builds the
// service holding the Context.
const Box = Context.Service('test/RealPipedProviderBox');
const BoxLayer = Effect.map(Effect.context(), (context) => Box.of({ context })).pipe(
  Layer.effect(Box),
);
const AppLayer = Layer.provideMerge(BoxLayer, StoreLayer);
const program = Effect.gen(function* () {
  const box = yield* Box;
  const out = [];
  for (const service of box.context.mapUnsafe.values()) {
    if (typeof service[name] === 'function') out.push(service[name]());
  }
  return [(yield* Store).readOne(), out];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), ['r', ['u']]);
