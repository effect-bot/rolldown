import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schedule from 'effect/Schedule';

// `Effect.repeat` provides `Schedule.CurrentMetadata` to each attempt. From the
// second attempt on, its `output` is the Schedule's output: a string naming a
// method of a service the program holds. Only the scalar passes through the
// Schedule, never the service.
const Store = Context.Service('test/ScheduleMetadataOutputStore');

const program = Effect.gen(function* () {
  const store = yield* Store;
  const seen = [];
  yield* Effect.repeat(
    Effect.gen(function* () {
      const metadata = yield* Schedule.CurrentMetadata;
      if (typeof metadata.output === 'string') seen.push(store[metadata.output]());
    }),
    Schedule.map(Schedule.recurs(1), () => 'metadataOutputUsed'),
  );
  return seen;
});

assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(
      program,
      Layer.succeed(Store)({
        metadataOutputUsed: () => 'used',
        metadataOutputUnused: () => 'unused',
      }),
    ),
  ),
  ['used'],
);
