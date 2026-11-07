import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Predicate from 'effect/Predicate';
import * as Schema from 'effect/Schema';
import * as SchemaTransformation from 'effect/SchemaTransformation';

// A value a schema's own transformation produces flows as ordinary code: the
// decode callback returns a captured service, and the program calls a method
// on the decoded value. That method stays; the unread one is removed.
const Store = Context.Service('test/DecodeTransformationStore');
const layer = Layer.succeed(Store)({
  decodedUsed: () => 'decoded',
  decodedUnused: () => 'unused',
});
const program = Effect.gen(function* () {
  const store = yield* Store;
  const StoreSchema = Schema.declare((u) => Predicate.hasProperty(u, 'decodedUsed'));
  const StoreFromName = Schema.String.pipe(
    Schema.decodeTo(
      StoreSchema,
      SchemaTransformation.transform({ decode: () => store, encode: () => 'store' }),
    ),
  );
  return Schema.decodeUnknownSync(StoreFromName)('store').decodedUsed();
});
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 'decoded');
