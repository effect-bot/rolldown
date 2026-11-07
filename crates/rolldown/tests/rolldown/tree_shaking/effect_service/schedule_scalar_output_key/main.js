import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';
// A Schedule's output is the string naming a method of a service the program
// holds: `Effect.repeat` returns it, and `retryOrElse` hands it to its fallback.
// The service is never a Schedule input, output or Done value; only the scalar
// is. Each method a string names is read and stays; the others are removed.
const RepeatStore = Context.Service('test/ScheduleOutputRepeat');
const RetryStore = Context.Service('test/ScheduleOutputRetry');
const program = Effect.gen(function* () {
  const repeatStore = yield* RepeatStore;
  const retryStore = yield* RetryStore;
  // The repeated effect succeeds with a string that names no method, so only
  // the Schedule's output can name `repeatUsed`.
  const repeatKey = yield* Effect.repeat(
    Effect.succeed('none'),
    Schedule.map(Schedule.recurs(1), () => 'repeatUsed'),
  );
  const retried = yield* Effect.retryOrElse(
    Effect.fail('always'),
    Schedule.map(Schedule.recurs(1), () => 'retryUsed'),
    (_error, retryKey) => Effect.sync(() => retryStore[retryKey]()),
  );
  return [repeatStore[repeatKey](), retried];
});
assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.mergeAll(
        Layer.succeed(RepeatStore)({ repeatUsed: () => 'repeat', repeatUnused: () => 'unused' }),
        Layer.succeed(RetryStore)({ retryUsed: () => 'retry', retryUnused: () => 'unused' }),
      ),
    ),
  ),
  ['repeat', 'retry'],
);
