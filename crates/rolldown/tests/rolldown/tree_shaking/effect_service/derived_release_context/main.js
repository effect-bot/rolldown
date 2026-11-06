import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The acquired value is the Context; the release finalizer receives it
// (acquireRelease and scoped, derived from Effect's code).
const Store = Context.Service('test/RealReleaseContext');
const releaseUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const releaseUnused = () => releaseUnusedHelper();
    return Store.of({ readOne, releaseUnused });
  }),
);

// Calls, on every service of a Context, the method named at run time.
const name = ['release', 'Unused'].join('');
const callAll = (context) => {
  const out = [];
  out.push(Context.get(context, Store)[name]());
  return out;
};

const released = [];
const program = Effect.scoped(
  Effect.gen(function* () {
    yield* Effect.acquireRelease(Effect.context(), (context) =>
      Effect.sync(() => {
        released.push(...callAll(context));
      }),
    );
    return (yield* Store).readOne();
  }),
);

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'r');
assert.deepStrictEqual(released, ['u']);
