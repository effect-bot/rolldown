import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Tracer from 'effect/Tracer';
// `Tracer.make` returns the tracer it is given, and its `context` hook is
// public and generic: called directly, it returns what the primitive it runs
// evaluates to, here the service. The method called on that result is read and
// stays; the other is removed.
const Store = Context.Service('test/TracerContextDirect');
const tracer = Tracer.make({
  span: (options) => Tracer.nativeTracer.span(options),
  context: (primitive, fiber) => primitive['~effect/Effect/evaluate'](fiber),
});
const program = Effect.gen(function* () {
  const store = yield* Store;
  const primitive = {
    ['~effect/Effect/evaluate']: () => store,
  };
  return yield* Effect.withFiber((fiber) => {
    const context = tracer.context;
    if (context === undefined) return Effect.succeed('no context');
    return Effect.succeed(context(primitive, fiber).contextUsed());
  });
});
assert.strictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.succeed(Store)({ contextUsed: () => 'used', contextUnused: () => 'unused' }),
    ),
  ),
  'used',
);
