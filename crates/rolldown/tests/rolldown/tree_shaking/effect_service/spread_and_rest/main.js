import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Store's provider spreads an `impl` literal into a new literal with extra
// methods, like `FileSystem.make`. The consumer destructures `readOne` and a
// rest. Nothing reads the rest, so `spreadImplUnused` goes from the impl
// literal and `spreadExtraUnused` from the provider. Enumerated's consumer
// enumerates its rest with `Object.keys`, which bails that key alone.
const Store = Context.Service('test/SpreadStore');
const Enumerated = Context.Service('test/SpreadEnumerated');
const spreadImplUnusedHelper = () => 'impl unused';
const enumeratedUnusedHelper = () => 'enumerated unused';

const impl = {
  readOne: () => 'read',
  spreadImplUnused: () => spreadImplUnusedHelper(),
};
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ ...impl, spreadExtra: () => 'extra', spreadExtraUnused: () => 'extra unused' }),
);
const EnumeratedLayer = Layer.succeed(
  Enumerated,
  Enumerated.of({ enumeratedRead: () => 'e', enumeratedUnused: () => enumeratedUnusedHelper() }),
);

const program = Effect.gen(function* () {
  const { readOne, ...rest } = yield* Store;
  const { enumeratedRead, ...enumeratedRest } = yield* Enumerated;
  return [readOne(), rest.spreadExtra(), enumeratedRead(), Object.keys(enumeratedRest)];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, EnumeratedLayer))),
  ['read', 'extra', 'e', ['enumeratedUnused']],
);
