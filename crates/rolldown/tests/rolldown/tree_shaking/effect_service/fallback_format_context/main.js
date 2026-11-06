import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { format } from 'effect/Formatter';
import * as Layer from 'effect/Layer';

// Formatter.format prints the service the program reads out of its Context.
// Effect's formatting is treated like console.log: walking the service's keys
// to print it keeps nothing, so the unread method goes and the printed output
// may shrink. The program asserts only on the method it reads.
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

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  return [(yield* Store).readOne(), format(Context.get(context, Store)).includes('readOne')];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ['r', true]);
