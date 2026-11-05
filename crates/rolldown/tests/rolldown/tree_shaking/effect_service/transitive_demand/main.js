import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `writeOne` is the only reader of `dependency.write`. Once `writeOne` is
// removed, `write` is no longer demanded from the Dependency provider either.
const Store = Context.Service('test/TransitiveStore');
const Dependency = Context.Service('test/TransitiveDependency');

const dependencyRead = () => 'read';
const dependencyWrite = () => 'write';

const DependencyLayer = Layer.succeed(Dependency, { read: dependencyRead, write: dependencyWrite });

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const dependency = yield* Dependency;
    const readOne = () => dependency.read();
    const writeOne = () => dependency.write();
    return Store.of({ readOne, writeOne });
  }),
);

const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.readOne();
});

const layer = Layer.provide(StoreLayer, DependencyLayer);
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'read');
