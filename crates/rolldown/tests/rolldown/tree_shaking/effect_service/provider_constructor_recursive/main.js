import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A constructor that returns its own result for a smaller argument. Following
// its returns must stop at the cycle; the program terminates at run time and
// the unread method goes.
const Store = Context.Service('test/RealProviderConstructorRecursive');

const recursiveUnusedHelper = () => 'unused';

const make = Effect.fnUntraced(function* (n) {
  if (n === 0) return { readOne: () => 'read', recursiveUnused: () => recursiveUnusedHelper() };
  return yield* make(n - 1);
});

const StoreLayer = Layer.effect(Store, make(1));

const program = Effect.gen(function* () {
  return (yield* Store).readOne();
});

assert.strictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), 'read');
