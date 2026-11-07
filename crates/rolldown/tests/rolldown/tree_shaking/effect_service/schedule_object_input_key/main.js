import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';
// A Schedule's only input is a plain object holding a string, and its callback
// calls the method of a captured service that the string names. The object
// holds no service, so the Schedule may receive it: the named method stays and
// the other is removed.
const Store = Context.Service('test/ScheduleObjectInputStore');
const calls = [];
const program = Effect.gen(function* () {
  const store = yield* Store;
  const schedule = Schedule.fromStep(
    Effect.succeed((_now, input) =>
      Effect.flatMap(
        Effect.sync(() => calls.push(store[input.key]())),
        () => Cause.done(0),
      ),
    ),
  );
  yield* Effect.repeat(Effect.succeed({ key: 'objectKeyUsed' }), schedule);
});
await Effect.runPromise(
  Effect.provide(
    program,
    Layer.succeed(Store)({ objectKeyUsed: () => 'used', objectKeyUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(calls, ['used']);
