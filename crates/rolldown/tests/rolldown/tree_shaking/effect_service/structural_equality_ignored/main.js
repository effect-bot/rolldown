import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Data from 'effect/Data';
import * as Effect from 'effect/Effect';
import * as Equal from 'effect/Equal';
import * as Hash from 'effect/Hash';
import * as Layer from 'effect/Layer';

// Structural Equal.equals and Hash.hash walk plain objects, arrays and Data
// values field by field without reading the Walked service's methods, so its
// unread method goes. Owned defines its own [Equal.symbol] and [Hash.symbol],
// which equality and hashing call: the results below depend on them.
const Walked = Context.Service('test/EqualityWalked');
const Owned = Context.Service('test/EqualityOwned');
const WalkedLayer = Layer.succeed(Walked, {
  readOne: () => 'r',
  equalUnused: () => 'e',
});
const OwnedLayer = Layer.succeed(Owned, {
  readTwo: () => 't',
  [Equal.symbol]: () => true,
  [Hash.symbol]: () => 7,
  ownedUnused: () => 'o',
});
// Not a service: an Equal value with the same hash, so Equal.equals calls
// Owned's own [Equal.symbol].
const twin = { [Equal.symbol]: () => true, [Hash.symbol]: () => 7 };
class Box extends Data.Class {}

const program = Effect.gen(function* () {
  const walked = yield* Walked;
  const owned = yield* Owned;
  return [
    walked.readOne(),
    owned.readTwo(),
    Equal.equals({ walked }, { walked }),
    Equal.equals([walked], [walked]),
    Equal.equals(new Box({ walked }), new Box({ walked })),
    typeof Hash.hash({ walked }),
    Equal.equals(owned, twin),
    Hash.hash(owned),
  ];
});

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(WalkedLayer, OwnedLayer))),
  ['r', 't', true, true, true, 'number', true, 7],
);
