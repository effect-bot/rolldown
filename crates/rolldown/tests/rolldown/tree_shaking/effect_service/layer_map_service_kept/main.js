import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as LayerMap from 'effect/LayerMap';

// A user class extending `LayerMap.Service`. Only `Context.Service` and
// `Context.Reference` classes are service keys, so none of the LayerMap's
// methods may be removed, although the program calls only `invalidate`.
class Greeter extends Context.Service()('test/LayerMapServiceKept/Greeter') {}

class Greeters extends LayerMap.Service()('test/LayerMapServiceKept', {
  lookup: (name) => Layer.succeed(Greeter, { greet: () => `hello ${name}` }),
}) {}

const program = Effect.gen(function* () {
  const greeters = yield* Greeters;
  yield* greeters.invalidate('world');
  return 'invalidated';
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, Greeters.layer)), 'invalidated');
