import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A service built from the Context holds it, and its Layer is a const passed
// to Effect.provide: the program reads the Context out of the service and
// calls, on every service in it, the method named at run time.
const Store = Context.Service('test/RealServiceHoldsContext');
const heldUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const heldUnused = () => heldUnusedHelper();
    return Store.of({ readOne, heldUnused });
  }),
);

const name = String(process.env.EFFECT_FIXTURE_METHOD ?? 'heldUnused');

const Box = Context.Service('test/RealServiceHoldsContextBox');
const BoxLayer = Layer.effect(
  Box,
  Effect.map(Effect.context(), (context) => Box.of({ context })),
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
