import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Equal from 'effect/Equal';
import * as Hash from 'effect/Hash';
import * as Layer from 'effect/Layer';
import * as Redactable from 'effect/Redactable';

// The protocol methods are keyed by effect's constants and read sibling
// methods through `this`; only effect calls them. Nothing else reads the
// siblings, so they stay only if the walks find the protocol methods.
const Keys = Context.Service('test/ConstantProtocolKeys');
const layer = Layer.succeed(Keys, {
  equalRead: () => true,
  hashRead: () => 7,
  redactRead: () => 'redacted',
  [Equal.symbol]() {
    return this.equalRead();
  },
  [Hash.symbol]() {
    return this.hashRead();
  },
  [Redactable.symbolRedactable]() {
    return this.redactRead();
  },
  keysUnused: () => 'unused',
});
// Not a service: an Equal value with the same hash, so Equal.equals calls the
// service's own [Equal.symbol].
const twin = { [Equal.symbol]: () => true, [Hash.symbol]: () => 7 };

const program = Effect.gen(function* () {
  const keys = yield* Keys;
  return [Equal.equals(keys, twin), Hash.hash(keys), Redactable.redact(keys)];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  true,
  7,
  'redacted',
]);
