import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// runPromise resolves with the Context the Effect succeeds with (derived from
// Effect's code: FiberImpl, addObserver, Promise).
const Store = Context.Service('test/RealRunPromiseContext');
const runPromiseUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const runPromiseUnused = () => runPromiseUnusedHelper();
    return Store.of({ readOne, runPromiseUnused });
  }),
);

// Calls, on every service of a Context, the method named at run time.
const name = ['runPromise', 'Unused'].join('');
const callAll = (context) => {
  const out = [];
  for (const service of context.mapUnsafe.values()) {
    if (typeof service[name] === 'function') out.push(service[name]());
  }
  return out;
};

const context = await Effect.runPromise(Effect.provide(Effect.context(), StoreLayer));
assert.deepStrictEqual(callAll(context), ['u']);
