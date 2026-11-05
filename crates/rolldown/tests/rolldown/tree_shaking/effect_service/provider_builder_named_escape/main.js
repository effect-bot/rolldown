import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Retention: a named function expression builder exposes itself through its
// inner name, so another caller gets an object from the same literal and reads
// `builderNamedUnused` on it. Demand on the literal keeps that method.
const Store = Context.Service('test/RealProviderBuilderNamedEscape');

let exposed;
const build = function inner() {
  exposed = inner;
  return { readOne: () => 'read', builderNamedUnused: () => 'unused' };
};

const make = Effect.fnUntraced(function* () {
  return build();
});

const StoreLayer = Layer.effect(Store, make());

const program = Effect.gen(function* () {
  return (yield* Store).readOne();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read');
assert.strictEqual(exposed().builderNamedUnused(), 'unused');
