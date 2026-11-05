import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A Context holding two services is printed and serialized. Effect's own
// `toJSON`/inspect on the Context enumerate the entries, but a formatter reads
// only `toJSON` and `[util.inspect.custom]` on them. `toJSON` of Store stays;
// the uncalled ordinary methods of both services go.
const Store = Context.Service('test/ContextFormatStore');
const Other = Context.Service('test/ContextFormatOther');
const contextFormatUnusedHelper = () => 'u1';
const contextFormatOtherUnusedHelper = () => 'u2';
const StoreLayer = Layer.succeed(
  Store,
  Store.of({
    readOne: () => 'r1',
    contextFormatUnused: () => contextFormatUnusedHelper(),
    toJSON: () => ({ store: true }),
  }),
);
const OtherLayer = Layer.succeed(
  Other,
  Other.of({
    readOther: () => 'r2',
    contextFormatOtherUnused: () => contextFormatOtherUnusedHelper(),
  }),
);

const program = Effect.gen(function* () {
  const context = yield* Effect.context();
  console.log(context);
  const json = JSON.parse(JSON.stringify(context));
  const stores = json.services.filter((entry) => entry.key === 'test/ContextFormatStore');
  return [(yield* Store).readOne(), (yield* Other).readOther(), stores[0].value];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(StoreLayer, OtherLayer))),
  ['r1', 'r2', { store: true }],
);
