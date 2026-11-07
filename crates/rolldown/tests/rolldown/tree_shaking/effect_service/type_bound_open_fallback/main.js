import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Struct from 'effect/Struct';
// A parameter the type resolver cannot bound stays open. Struct.evolve's evolver is
// a type parameter constrained by a mapped type, so the resolver finds no bound;
// a service passed as the evolver reaches the API, which calls its count method.
const Evolver = Context.Service('test/OpenEvolver');
const layer = Layer.succeed(Evolver)({ count: (n) => n + 1 });
const program = Effect.gen(function* () {
  return Struct.evolve({ count: 1 }, yield* Evolver).count;
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 2);
