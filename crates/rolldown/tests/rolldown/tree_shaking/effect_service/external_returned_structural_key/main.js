import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { hold, identity, take } from './late.js';

// Structural keys (`{ key }` records, which effect reads by their `key`) handed
// to an external module and handed back. Two hold names only known at run time:
// one holds its name when it is handed over, the other is handed over empty and
// given its `key` afterwards. Tim's decision (2026-11-09): a runtime name in a
// program key object's `key` field is not tracked once the object is handed to
// code the pass can't see, so those two lookups keep nothing and both of their
// method definitions go. The ground is rarity (programs seldom build a key by
// hand from a runtime name and route it through unseen code), not that unseen
// code could make the value: the object is the program's own. A literal-named
// structural key is still covered: its read method stays, its unread one goes.
const Held = Context.Service('test/ExternalStructuralHeld');
const Late = Context.Service('test/ExternalStructuralLate');
const Literal = Context.Service('test/ExternalStructuralLiteral');

const late = {};
hold(late);
late.key = process.env.EXTERNAL_STRUCTURAL_LATE_NAME;

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [
    Context.get(context, identity({ key: process.env.EXTERNAL_STRUCTURAL_HELD_NAME })).heldUsed(),
    Context.get(context, take()).lateUsed(),
    Context.get(context, identity({ key: 'test/ExternalStructuralLiteral' })).literalUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(Held, Held.of({ heldUsed: () => 'held', heldUnused: () => 'unused' })),
  Layer.succeed(Late, Late.of({ lateUsed: () => 'late', lateUnused: () => 'unused' })),
  Layer.succeed(
    Literal,
    Literal.of({ literalUsed: () => 'literal', literalUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'held',
  'late',
  'literal',
]);
