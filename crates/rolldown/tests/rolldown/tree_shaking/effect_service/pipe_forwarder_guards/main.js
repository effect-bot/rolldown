import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { pipeArguments } from 'effect/Pipeable';
import { lookalike } from './lookalike.js';

// Calls a pipe model must not get wrong: a forwarder called with another
// receiver through `.call`, a forwarder given spread functions, and a
// lookalike whose `pipeArguments` is a local function. Each reads one method
// of the service, which stays; the method nothing reads is removed.
const Proto = {
  pipe() {
    return pipeArguments(this, arguments);
  },
};

const Guarded = Context.Service('test/PipeGuards');
const guardHelper = () => 'unused';
const GuardedLayer = Layer.succeed(Guarded, {
  viaCall: () => 'c',
  viaSpread: () => 's',
  viaLookalike: () => 'l',
  guardUnused: () => guardHelper(),
});

const program = Effect.gen(function* () {
  const guarded = yield* Guarded;
  const held = Object.create(Proto);
  held.value = guarded;
  const fns = [(x) => x.value.viaSpread()];
  return [
    Proto.pipe.call({ value: guarded }, (x) => x.value.viaCall()),
    held.pipe(...fns),
    lookalike(guarded).pipe((x) => x.viaLookalike()),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, GuardedLayer)), [
  'c',
  's',
  'l',
]);
