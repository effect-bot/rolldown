import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';
import * as Tuple from 'effect/Tuple';

// Option and Tuple are summarised from their types: each export returns the key
// it was handed, at a type parameter the caller picks. The program looks the
// services up through those results, so each lookup must reach the service's
// implementation: the method read there must stay. A key handed to
// type-modelled code also bails its key today, so the unread methods are not
// checked. Other, looked up directly, prunes as usual.
const ViaOption = Context.Service('test/TypedReturnedOption');
const ViaTuple = Context.Service('test/TypedReturnedTuple');
const Other = Context.Service('test/TypedReturnedOther');

const program = Effect.gen(function* () {
  const other = yield* Other;
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  const optionKey = Option.getOrThrow(Option.some(ViaOption));
  const [tupleKey] = Tuple.make(ViaTuple, 1);
  return [
    Context.get(context, optionKey).optionUsed(),
    Context.get(context, tupleKey).tupleUsed(),
    other.otherUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    ViaOption,
    ViaOption.of({ optionUsed: () => 'option', optionUnused: () => 'unused' }),
  ),
  Layer.succeed(ViaTuple, ViaTuple.of({ tupleUsed: () => 'tuple', tupleUnused: () => 'unused' })),
  Layer.succeed(Other, Other.of({ otherUsed: () => 'other', otherUnused: () => 'unused' })),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'option',
  'tuple',
  'other',
]);
