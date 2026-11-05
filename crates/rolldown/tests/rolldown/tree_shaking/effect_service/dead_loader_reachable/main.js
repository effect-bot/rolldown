import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A code loader that runs. It is handed no service, no Context and no Effect
// primitive, and its result is a number, so it can reach no service: the
// unread method is removed.
const Store = Context.Service('test/RealDeadLoaderReachable');
const liveUnusedHelper = () => 'liveUnused';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'readOne';
    const liveUnused = () => liveUnusedHelper();
    return Store.of({ readOne, liveUnused });
  }),
);

const called = () => new Function('return 1')();

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), called()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'readOne',
  1,
]);
