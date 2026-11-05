import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { build } from './client.js';

// The function that builds the provider's object is also called directly, and
// the direct caller reads a method by a name folded from constants. Demand on
// the shared literal keeps `sharedRead`; `sharedUnused`, read by nobody, goes.
class Client extends Context.Service()('test/RealProviderConstructorShared') {}

const make = Effect.fnUntraced(function* () {
  return build();
});

const ClientLayer = Layer.effect(Client, make());

const program = Effect.gen(function* () {
  const client = yield* Client;
  const direct = build();
  const name = ['shared', 'Read'].join('');
  return [client.get(), direct[name]()];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, ClientLayer)), [
  'got',
  'read directly',
]);
