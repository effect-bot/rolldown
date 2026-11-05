import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

import * as Fiber from 'effect/Fiber';

// A forked and joined Fiber, kept after its scope ends, cannot observe a
// service, so the pass must still remove `writeOne`.
const Store = Context.Service('test/RealRetainedFiber');

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

let retained;
const program = Effect.gen(function* () {
  const fiber = yield* Effect.forkChild(
    Effect.gen(function* () {
      return (yield* Store).readOne();
    }),
  );
  retained = fiber;
  return yield* Fiber.join(fiber);
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read');
assert.ok(retained !== undefined);
