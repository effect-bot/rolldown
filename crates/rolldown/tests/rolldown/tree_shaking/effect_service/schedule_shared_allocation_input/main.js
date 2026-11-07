import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';
// Two records come from one allocation site (the same `map` callback). The
// first is provided as an unrelated Config service; the second, holding only
// a string, is the Schedule's input, and its callback calls the method of a
// captured service that the string names. Only the first record is a
// service, so the second may enter the Schedule: the named method stays and
// the other is removed.
const Config = Context.Service('test/SharedAllocationConfig');
const Store = Context.Service('test/SharedAllocationStore');
const boxes = ['config', 'sharedUsed'].map((key) => ({ key }));
const supplied = boxes[0];
const input = boxes[1];
assert.ok(supplied !== undefined && input !== undefined && supplied !== input);
const calls = [];
const program = Effect.gen(function* () {
  const config = yield* Config;
  const store = yield* Store;
  const schedule = Schedule.fromStep(
    Effect.succeed((_now, box) =>
      Effect.flatMap(
        Effect.sync(() => calls.push(store[box.key]())),
        () => Cause.done(0),
      ),
    ),
  );
  yield* Effect.repeat(Effect.succeed(input), schedule);
  return config.key;
});
assert.strictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.mergeAll(
        Layer.succeed(Config)(supplied),
        Layer.succeed(Store)({ sharedUsed: () => 'used', sharedUnused: () => 'unused' }),
      ),
    ),
  ),
  'config',
);
assert.deepStrictEqual(calls, ['used']);
