import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// An Effect fails with the Context as its error; the handler reads it.
const Store = Context.Service('test/RealFailureContext');
const failureUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const failureUnused = () => failureUnusedHelper();
    return Store.of({ readOne, failureUnused });
  }),
);

// Calls, on every service of a Context, the method named at run time.
const name = ['failure', 'Unused'].join('');
const callAll = (context) => {
  const out = [];
  out.push(Context.get(context, Store)[name]());
  return out;
};

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  const read = yield* Effect.fail(context).pipe(
    Effect.catch((error) => Effect.sync(() => callAll(error))),
  );
  return [(yield* Store).readOne(), read];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['r', ['u']]);
