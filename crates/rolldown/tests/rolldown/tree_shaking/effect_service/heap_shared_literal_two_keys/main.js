import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// One factory literal is provided under two keys; each key reads a different
// method, so the literal keeps both however its copies are separated.
const pinA = () => 'a';
const pinB = () => 'b';
const pinUnused = () => 'unused';
const make = (tag) => ({ tag, pinA, pinB, pinUnused });

const First = Context.Service('test/SharedLiteralFirst');
const Second = Context.Service('test/SharedLiteralSecond');
const layer = Layer.merge(
  Layer.succeed(First, make('first')),
  Layer.succeed(Second, make('second')),
);

const program = Effect.gen(function* () {
  return [(yield* First).pinA(), (yield* Second).pinB()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['a', 'b']);
