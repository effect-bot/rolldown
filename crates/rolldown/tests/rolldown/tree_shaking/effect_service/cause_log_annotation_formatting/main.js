import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A service logged as a log annotation by the built-in logger: Effect's
// formatting keeps nothing, so the unread method goes. The printed output may
// shrink; the program asserts only on its shape. (No service goes into a
// Cause: services never enter the error channel.)
const Walked = Context.Service('test/CauseAnnotationWalked');
const WalkedLayer = Layer.succeed(Walked, {
  readOne: () => 'r',
  causeUnused: () => 'c',
});

const lines = [];
const program = Effect.gen(function* () {
  const walked = yield* Walked;
  yield* Effect.log('annotated').pipe(Effect.annotateLogs('walked', walked));
  return walked.readOne();
});

const saved = console.log;
console.log = (...args) => lines.push(args);
const result = await Effect.runPromise(Effect.provide(program, WalkedLayer)).finally(() => {
  console.log = saved;
});
assert.strictEqual(result, 'r');
assert.strictEqual(lines.length, 1);
