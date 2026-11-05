import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { build } from './client.js';

// An arbitrary pipeable of the constructor receives the Effect that builds the
// object and reads a method by a name folded from constants. That read is
// static, so `pipeableRead` stays; `pipeableUnused`, which nothing reads, goes.
class Client extends Context.Service()('test/RealProviderConstructorPipeableEscape') {}

const observed = [];

const make = Effect.fnUntraced(
  function* () {
    return build();
  },
  (effect) =>
    Effect.tap(effect, (client) =>
      Effect.sync(() => observed.push(client[['pipeable', 'Read'].join('')]())),
    ),
);

const ClientLayer = Layer.effect(Client, make());

const program = Effect.gen(function* () {
  const client = yield* Client;
  return client.get();
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, ClientLayer)), 'got');
assert.deepStrictEqual(observed, ['read by the pipeable']);
