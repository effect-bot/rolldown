import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// The Schedule's callbacks return Effects, and those Effects call the service.
// What the callbacks return is a Schedule's output or condition, which holds no
// service, but the callbacks and their Effects still run. Only
// `returnedUnused` is read nowhere.
const Store = Context.Service('test/ScheduleCallbackReturnedEffect');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    mapUsed: () => 'mapped',
    whileUsed: () => true,
    returnedUnused: () => 'unused',
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const schedule = Schedule.recurs(1).pipe(
    Schedule.map(() => Effect.sync(() => store.mapUsed())),
    Schedule.while(() =>
      Effect.gen(function* () {
        const again = yield* Store;
        return again.whileUsed();
      }),
    ),
  );
  return yield* Effect.repeat(Effect.succeed('done'), schedule);
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'mapped');
