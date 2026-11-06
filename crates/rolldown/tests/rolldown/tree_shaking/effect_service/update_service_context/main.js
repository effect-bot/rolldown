import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const Store = Context.Service('test/RealUpdateStore');
const updateUnusedHelper = () => 'u';
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => 'r';
    const updateUnused = () => updateUnusedHelper();
    return Store.of({ readOne, updateUnused });
  }),
);
const name = String(process.env.EFFECT_FIXTURE_METHOD ?? 'updateUnused');
// `Effect.updateService` replaces a service with one holding the Context.
const Box = Context.Service('test/RealUpdateServiceBox');
const BoxLayer = Layer.succeed(Box, Box.of({ context: undefined }));
const inner = Effect.gen(function* () {
  const box = yield* Box;
  const out = [];
  out.push(Context.get(box.context, Store)[name]());
  return [(yield* Store).readOne(), out];
});
const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  return yield* Effect.updateService(inner, Box, () => Box.of({ context }));
});
assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.merge(BoxLayer, StoreLayer))),
  ['r', ['u']],
);
