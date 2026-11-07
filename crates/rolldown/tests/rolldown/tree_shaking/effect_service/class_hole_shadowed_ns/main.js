import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as ns from './svc.js';

// A method parameter named `ns` shadows the namespace import: the method
// reads plain data, not the import, so Store keeps only the method the program
// reads itself. A second class references the import directly and reads
// Second's implementation through it: Second keeps every method.
const Store = Context.Service('test/ClassHoleShadowedNsStore');
const Second = Context.Service('test/ClassHoleShadowedNsSecond');
class Shadowing {
  run(ns) {
    return ns.impl.shadowedUsed();
  }
}
class Direct {
  run() {
    return ns.secondImpl.directFirst();
  }
}

const program = Effect.gen(function* () {
  const store = yield* Store;
  yield* Second;
  return [
    new Shadowing().run({ impl: { shadowedUsed: () => 'plain' } }),
    new Direct().run(),
    store.shadowedUsed(),
  ];
});

const layer = Layer.mergeAll(Layer.succeed(Store, ns.impl), Layer.succeed(Second, ns.secondImpl));
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'plain',
  'first',
  'used',
]);
