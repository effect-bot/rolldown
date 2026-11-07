import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import { format, formatJson } from 'effect/Formatter';
import * as Layer from 'effect/Layer';

// Effect's own formatting is treated like console.log: the built-in logger and
// a span attribute walk the Walked service without reading its methods, so its
// unread method goes. (No service goes into a Cause: services never enter the
// error channel.)
// Printed defines toString and toJSON, which Formatter.format and formatJson
// call: those stay, its unread method goes.
const Walked = Context.Service('test/FormattingWalked');
const Printed = Context.Service('test/FormattingPrinted');
const WalkedLayer = Layer.succeed(Walked, {
  readOne: () => 'r',
  walkUnused: () => 'w',
});
const PrintedLayer = Layer.succeed(Printed, {
  readTwo: () => 't',
  toString: () => 'printed',
  toJSON: () => 'json',
  printUnused: () => 'p',
});

const lines = [];
const program = Effect.gen(function* () {
  const walked = yield* Walked;
  const printed = yield* Printed;
  yield* Effect.log(walked).pipe(Effect.withSpan('format', { attributes: { walked } }));
  return [walked.readOne(), printed.readTwo(), format(printed), formatJson(printed)];
});

const saved = console.log;
console.log = (...args) => lines.push(args);
const result = await Effect.runPromise(
  Effect.provide(program, Layer.mergeAll(WalkedLayer, PrintedLayer)),
).finally(() => {
  console.log = saved;
});
assert.deepStrictEqual(result, ['r', 't', 'printed', '"json"']);
assert.strictEqual(lines.length, 1);
