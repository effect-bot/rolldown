import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A key class that extends another key class: it inherits the base's key, so a
// service provided through the base is read through the subclass. The method
// the program calls stays; nothing reads `subclassUnused`.
class BaseStore extends Context.Service()('test/KeyClassSubclass') {}
class Store extends BaseStore {}
const subclassUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(BaseStore, {
  subclassUsed: () => 'used',
  subclassUnused: () => subclassUnusedHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.subclassUsed();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'used');
