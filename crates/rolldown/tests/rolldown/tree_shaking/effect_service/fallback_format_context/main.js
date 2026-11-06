import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { format } from 'effect/Formatter';
import * as Layer from 'effect/Layer';

// Formatter.format, which the pass analyses generically, prints every method
// of the service the program reads out of its Context: the output must not
// change.
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

const name = ['format', 'Unused'].join('');

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  return [(yield* Store).readOne(), format(Context.get(context, Store)).includes(name)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['r', true]);
