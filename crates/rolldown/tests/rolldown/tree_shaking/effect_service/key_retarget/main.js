import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Keys retargeted by writing their `key` string: after this, yielding Helper
// or Computed finds the services provided for the target keys. Whether the
// analysis follows the write precisely or lets the written key alias every
// key, nothing reads the `*Unused` methods, so they go.
const Helper = Context.Service('test/Helper');
const Computed = Context.Service('test/Computed');
const HelperTarget = Context.Service('test/HelperTarget');
const ComputedTarget = Context.Service('test/ComputedTarget');

function retarget(key, to) {
  key.key = to;
}
retarget(Helper, 'test/HelperTarget');
Computed[['key'].join('')] = 'test/ComputedTarget';

const layer = Layer.mergeAll(
  Layer.succeed(HelperTarget, { retargetRead: () => 1, retargetHelperUnused: () => 2 }),
  Layer.succeed(ComputedTarget, { retargetRead: () => 3, retargetComputedUnused: () => 4 }),
);

const program = Effect.gen(function* () {
  return [(yield* Helper).retargetRead(), (yield* Computed).retargetRead()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [1, 3]);
