import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/RealLookupserviceStore');
const lookupServiceUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const lookupServiceUnused = () => lookupServiceUnusedHelper();
    return Store.of({ readOne, lookupServiceUnused });
  }),
);
const name = ['lookupService', 'Unused'].join('');
const Box = Context.Service('test/RealLookupServiceBox');
const BoxLayer = Layer.effect(
  Box,
  Effect.map(Effect.context(), (context) => Box.of({ context })),
);
// `Effect.service` of a key whose service holds the Context.
const AppLayer = Layer.provideMerge(BoxLayer, StoreLayer);
const program = Effect.service(Box).pipe(
  Effect.flatMap((box) =>
    Effect.gen(function* () {
      const out = [];
      for (const service of box.context.mapUnsafe.values()) {
        if (typeof service[name] === 'function') out.push(service[name]());
      }
      return [(yield* Store).readOne(), out];
    }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), ['r', ['u']]);
