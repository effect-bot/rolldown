import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { Store } from './lib.js';

const program = Effect.gen(function* () {
  return (yield* Store).dormantUnsafeUsed();
});

const layer = Layer.succeed(
  Store,
  Store.of({ dormantUnsafeUsed: () => 'used', dormantUnsafeUnread: () => 'unread' }),
);
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'used');
