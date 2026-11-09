import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';
// Each record has a distinct, once-only top-level allocation site.
// Only boxes[0] is provided; boxes[1] is a service-free Schedule input. The
// method its string names stays and the bundle runs. The other may stay: the
// step runs with an unknown input, so the read is an unknown key.
const Config = Context.Service('test/ArrayProvidedConfig');
const Store = Context.Service('test/ArrayProvidedStore');
const boxes = [{ key: 'config' }, { key: 'arrayUsed' }];
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
        Layer.succeed(Store)({ arrayUsed: () => 'used', arrayUnused: () => 'unused' }),
      ),
    ),
  ),
  'config',
);
assert.deepStrictEqual(calls, ['used']);
