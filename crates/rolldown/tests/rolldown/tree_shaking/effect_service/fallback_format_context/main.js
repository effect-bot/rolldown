import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { format } from 'effect/Formatter';
import * as Layer from 'effect/Layer';

// Formatter.format, which the pass does not derive, prints every method of
// every service of the Context it is given: the output must not change.
const Store = Context.Service('test/RealFormatContext');
const formatUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const formatUnused = () => formatUnusedHelper();
    return Store.of({ readOne, formatUnused });
  }),
);

// Calls, on every service of a Context, the method named at run time.
const name = ['format', 'Unused'].join('');
const callAll = (context) => {
  const out = [];
  for (const service of context.mapUnsafe.values()) {
    if (typeof service[name] === 'function') out.push(service[name]());
  }
  return out;
};

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  return [(yield* Store).readOne(), format(context).includes(name)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['r', true]);
