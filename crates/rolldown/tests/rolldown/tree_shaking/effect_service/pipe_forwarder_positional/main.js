import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { pipeArguments } from 'effect/Pipeable';

// A service with a bundled pipe forwarder: `store.pipe(f, g)` runs `f` on the
// service and `g` on what `f` returns. Only `f` sees the service, so the
// service's `step`, which only `g` reads by name on its own input, is removed.
const Store = Context.Service('test/PipePositional');
const stepHelper = () => 'unused';
const result = { step: () => 's' };
const StoreLayer = Layer.succeed(Store, {
  pipe() {
    return pipeArguments(this, arguments);
  },
  first: () => result,
  step: () => stepHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.pipe(
    (s) => s.first(),
    (r) => r.step(),
  );
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 's');
