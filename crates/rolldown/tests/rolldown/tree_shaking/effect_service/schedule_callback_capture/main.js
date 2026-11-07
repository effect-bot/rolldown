import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// Schedule callbacks still run when the Schedule carries no service: each
// `Schedule.map` callback here calls a method of the service it closes over.
// `Schedule.recurs(0)` finishes at once, so each callback runs only when the
// Schedule completes (its `Done` signal). One callback returns a plain value,
// the other an Effect, which Schedule maps with Effect's internal `map`. Each
// method stays and the bundle runs; the method nothing reads is removed.
const Store = Context.Service('test/ScheduleCallbackCapture');
const scheduleUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store)({
  onDone: () => 'plain',
  onDoneEffect: () => 'effect',
  scheduleUnused: () => scheduleUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  const log = [];
  yield* Effect.repeat(
    Effect.succeed(0),
    Schedule.map(Schedule.recurs(0), () => {
      log.push(store.onDone());
    }),
  );
  yield* Effect.repeat(
    Effect.succeed(0),
    Schedule.map(Schedule.recurs(0), () =>
      Effect.sync(() => {
        log.push(store.onDoneEffect());
      }),
    ),
  );
  return log;
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'plain',
  'effect',
]);
