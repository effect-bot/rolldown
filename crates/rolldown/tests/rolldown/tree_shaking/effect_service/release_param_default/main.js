import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The release callback reads the acquired object through `arguments` in a
// later parameter's default value, not in its body.
const K = Context.Service('test/ReleaseParamDefault');
const log = [];
const releaseDefaultUnusedHelper = () => 3;

const layer = Layer.effect(
  K,
  Effect.acquireRelease(
    Effect.succeed({
      read: () => 1,
      releaseDefaultBase: () => 2,
      releaseDefaultUnused: () => releaseDefaultUnusedHelper(),
    }),
    function (s, exit, value = arguments[0].releaseDefaultBase()) {
      return Effect.sync(() => log.push(value));
    },
  ),
);

const program = Effect.gen(function* () {
  return (yield* K).read();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 1);
assert.deepStrictEqual(log, [2]);
