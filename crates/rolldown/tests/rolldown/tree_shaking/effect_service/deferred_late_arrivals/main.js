import assert from 'node:assert';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { Store, callerA, callerB, relay } from './lib.js';
import { first, second } from './reversed.js';

const program = Effect.gen(function* () {
  const b = yield* callerB();
  const a = yield* callerA();
  const all = yield* Effect.all([
    relay(() => Effect.succeed(1)),
    relay(() => Effect.succeed(2)),
    relay(() => Effect.succeed(3)),
  ]);
  return [a, b, ...all, yield* second(), yield* first()];
});

const layer = Layer.succeed(
  Store,
  Store.of({
    lateA: () => 'a',
    lateB: () => 'b',
    lateC: () => 'c',
    lateD: () => 'd',
    lateUnread: () => 'unread',
  }),
);

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'a',
  'b',
  1,
  2,
  3,
  'd',
  'c',
]);
