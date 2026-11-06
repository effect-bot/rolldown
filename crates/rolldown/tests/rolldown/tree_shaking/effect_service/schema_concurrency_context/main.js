import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Schema from 'effect/Schema';
import * as SchemaTransformation from 'effect/SchemaTransformation';

// A real Context reaches code that Schema's internal helpers run: the decode
// gets the fiber's Context with `Effect.context()` and reads one service from
// it with `Context.get`. That read is the demand; the other method goes.
const Store = Context.Service('test/SchemaContextStore');

const StoreLayer = Layer.succeed(
  Store,
  Store.of({ readOne: (s) => `read ${s}`, writeOne: () => 'write' }),
);

const ReadThroughStore = Schema.String.pipe(
  Schema.decodeTo(
    Schema.String,
    SchemaTransformation.transformEffect({
      decode: (s) =>
        Effect.map(Effect.context(), (context) => Context.get(context, Store).readOne(s)),
      encode: (s) => Effect.succeed(s),
    }),
  ),
);

const Item = Schema.Struct({
  first: ReadThroughStore,
  kind: Schema.Union([Schema.Literal('a'), Schema.Literal('b')]),
});

const program = Schema.decodeUnknownEffect(Item)(
  { first: 'x', kind: 'b' },
  { concurrency: 'unbounded' },
);

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), {
  first: 'read x',
  kind: 'b',
});
