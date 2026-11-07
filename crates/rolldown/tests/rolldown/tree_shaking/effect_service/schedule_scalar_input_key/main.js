import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';
// The Schedule's only input is the string "used", and its callback uses that
// string as the name of a method on the service it closes over. The service is
// never a Schedule input, output or Done value: only the scalar is. So the
// method the string names is read and stays; the other is removed.
const Store = Context.Service('test/ScheduleScalarInputStore');
const calls = [];
const program = Effect.gen(function* () {
  const store = yield* Store;
  const schedule = Schedule.fromStep(
    Effect.succeed((_now, input) =>
      Effect.flatMap(
        Effect.sync(() => calls.push(store[input]())),
        () => Cause.done(0),
      ),
    ),
  );
  yield* Effect.repeat(Effect.succeed('used'), schedule);
});
await Effect.runPromise(
  Effect.provide(
    program,
    Layer.succeed(Store)({ used: () => 'used', scheduleKeyUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(calls, ['used']);
