import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { forward } from './hole.js';

// A function in a module hole hands the service to a callback through
// `arguments`. The callback reads one method, which stays; the method nothing
// reads is removed.
const Forwarded = Context.Service('test/HoleArguments');
const forwardedHelper = () => 'unused';
const ForwardedLayer = Layer.succeed(Forwarded, {
  viaArguments: () => 'a',
  argumentsUnused: () => forwardedHelper(),
});

const program = Effect.gen(function* () {
  const forwarded = yield* Forwarded;
  return forward(forwarded, (x) => x.viaArguments());
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, ForwardedLayer)), 'a');
