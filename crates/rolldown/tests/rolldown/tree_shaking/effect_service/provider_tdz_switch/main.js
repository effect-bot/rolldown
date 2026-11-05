import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The cases of a switch share one scope. `helper` is declared earlier in the
// source, but entering `case 1` skips its initializer, so building the provider
// throws. Removing the unread property would drop that exception.
const Store = Context.Service('test/RealProviderTdzSwitch');

const program = Effect.gen(function* () {
  return (yield* Store).readOne();
});

let caught = false;
try {
  switch (Number('1')) {
    case 0:
      const helper = () => 'unused';
      break;
    case 1:
      const layer = Layer.succeed(Store, {
        readOne: () => 'read',
        tdzUnused: helper,
      });
      await Effect.runPromise(Effect.provide(program, layer));
      break;
  }
} catch (error) {
  caught = error instanceof ReferenceError;
}
assert.strictEqual(caught, true);
