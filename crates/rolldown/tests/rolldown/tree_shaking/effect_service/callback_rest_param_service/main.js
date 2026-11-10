import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';
// A callback declared to take one argument reads it through a rest parameter.
// The rest parameter receives every argument from its position on, so the
// service `Option.map` hands the callback reaches `xs[0]`: the method it calls
// stays and the bundle runs.
const Store = Context.Service('test/CallbackRestParam');
const program = Effect.gen(function* () {
  const store = yield* Store;
  return Option.map(Option.some(store), (...xs) => xs[0].restUsed());
});
assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.succeed(Store)({ restUsed: () => 'used', restUnused: () => 'unused' }),
    ),
  ),
  Option.some('used'),
);
