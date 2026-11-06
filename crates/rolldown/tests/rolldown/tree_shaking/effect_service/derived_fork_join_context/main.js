import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Fiber from 'effect/Fiber';
import * as Layer from 'effect/Layer';

// A child fiber succeeds with its Context; joining it hands the Context to
// the program (forkChild and Fiber.join, derived from Effect's code).
const Store = Context.Service('test/RealForkJoinContext');
const forkJoinUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const forkJoinUnused = () => forkJoinUnusedHelper();
    return Store.of({ readOne, forkJoinUnused });
  }),
);

// Calls, on every service of a Context, the method named at run time.
const name = ['forkJoin', 'Unused'].join('');
const callAll = (context) => {
  const out = [];
  out.push(Context.get(context, Store)[name]());
  return out;
};

const program = Effect.gen(function* () {
  const fiber = yield* Effect.forkChild(Effect.context());
  const context = yield* Fiber.join(fiber);
  return [(yield* Store).readOne(), callAll(context)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['r', ['u']]);
