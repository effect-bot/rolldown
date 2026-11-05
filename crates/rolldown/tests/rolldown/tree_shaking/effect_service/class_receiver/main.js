import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class heritage expression runs with the method's receiver, so `read`
// reads `this.classReceiverBase` even though it is inside a class.
const K = Context.Service('test/ClassReceiver');
const classReceiverUnusedHelper = () => 3;

const layer = Layer.effect(
  K,
  Effect.gen(function* () {
    return {
      read: function () {
        const C = class extends this.classReceiverBase() {};
        return new C().value;
      },
      classReceiverBase: () =>
        class {
          value = 2;
        },
      classReceiverUnused: () => classReceiverUnusedHelper(),
    };
  }),
);

const program = Effect.gen(function* () {
  return (yield* K).read();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 2);
