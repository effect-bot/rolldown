import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A service put in a Cause and printed with Cause.pretty, and a service
// logged as a log annotation by the built-in logger: Effect's formatting
// keeps nothing, so the unread method goes. The printed output may shrink;
// the program asserts only on its shape.
const Walked = Context.Service('test/CauseAnnotationWalked');
const WalkedLayer = Layer.succeed(Walked, {
  readOne: () => 'r',
  causeUnused: () => 'c',
});

const lines = [];
const program = Effect.gen(function* () {
  const walked = yield* Walked;
  yield* Effect.log('annotated').pipe(Effect.annotateLogs('walked', walked));
  const pretty = Cause.pretty(Cause.fail(walked));
  return [walked.readOne(), pretty.startsWith('Error')];
});

const saved = console.log;
console.log = (...args) => lines.push(args);
const result = await Effect.runPromise(Effect.provide(program, WalkedLayer)).finally(() => {
  console.log = saved;
});
assert.deepStrictEqual(result, ['r', true]);
assert.strictEqual(lines.length, 1);
