import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// `Schedule.map` calls the user callback and maps the Effect it returns with
// `internal/effect.js`'s `map`, the declaration behind the stable `Effect.map`.
// With a schedule, `Effect.repeat` succeeds with the schedule's output, so the
// service flows through that internal call to the program.
const Store = Context.Service('test/InternalCombinatorStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readOne: () => 'read', writeOne: () => 'write' }),
);

const program = Effect.gen(function* () {
  const store = yield* Effect.repeat(
    Effect.succeed(0),
    Schedule.map(Schedule.recurs(1), () => Effect.service(Store)),
  );
  return store.readOne();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read');
