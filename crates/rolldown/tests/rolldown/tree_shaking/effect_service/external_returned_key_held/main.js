import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { hold, take, unbox } from './late.js';

// The external module returns keys it reached through ordinary values: one
// inside an object handed to it, and one stored in an object it already holds
// after it was handed over. Each lookup through what it returns must reach the
// service's implementation, so the method read there must stay. Handing a key
// to unseen code bails nothing, so the unread methods still prune.
const Nested = Context.Service('test/ExternalReturnedNested');
const Held = Context.Service('test/ExternalReturnedHeld');

const box = {};
hold(box);
box.inner = Held;

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [
    Context.get(context, unbox({ inner: Nested })).nestedUsed(),
    Context.get(context, take()).heldUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(Nested, Nested.of({ nestedUsed: () => 'nested', nestedUnused: () => 'unused' })),
  Layer.succeed(Held, Held.of({ heldUsed: () => 'held', heldUnused: () => 'unused' })),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['nested', 'held']);
