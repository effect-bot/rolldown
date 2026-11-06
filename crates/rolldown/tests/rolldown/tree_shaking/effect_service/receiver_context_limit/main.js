import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// One method reads `this` on 65 receivers allocated at different sites, one more
// than the per-function context limit; the last receiver holds the service.
const proto = {
  describe() {
    return this.target.limitUsed();
  },
};
const plain = { limitUsed: () => 'plain' };

const Store = Context.Service('test/ReceiverLimitStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ limitUsed: () => 'used', unusedLimit: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const receivers = [
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: plain },
    { __proto__: proto, target: store },
  ];
  return receivers.map((receiver) => receiver.describe());
});
const out = await Effect.runPromise(Effect.provide(program, StoreLayer));
assert.strictEqual(out.length, 65);
assert.strictEqual(out[64], 'used');
