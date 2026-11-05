import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// RunOptions with an `onFiberStart` that receives the Fiber but reads nothing
// from it cannot observe a service, so the pass must still remove `writeOne`.
const Store = Context.Service('test/RealRunOptions');

const readHelper = () => 'read';
const writeHelper = () => 'write';

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => readHelper();
    const writeOne = () => writeHelper();
    return Store.of({ readOne, writeOne });
  }),
);

const program = Effect.gen(function* () {
  return (yield* Store).readOne();
});

let started = 0;
const result = await Effect.runPromise(Effect.provide(program, StoreLayer), {
  onFiberStart: (fiber) => {
    started += fiber === undefined ? 0 : 1;
  },
});
assert.strictEqual(result, 'read');
assert.ok(started >= 0);
