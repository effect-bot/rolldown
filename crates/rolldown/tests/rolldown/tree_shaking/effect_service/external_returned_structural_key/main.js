import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { hold, identity, take } from './late.js';

// Structural keys (`{ key }` records, which effect reads by their `key`) handed
// to an external module and handed back. Their names are only known at run time
// (the test sets the variables to the services' key names): one holds its name
// when it is handed over, the other is handed over empty and given its `key`
// afterwards. What comes back is the program's own key object, not a string
// unseen code made, so each lookup must reach the service's implementation and
// the method read there must stay. A literal-named structural key is the
// control (green today). Handing keys over bails nothing, so the unread methods
// still prune.
//
// Red, pending Tim's decision: the pass reads a structural key handed to unseen
// code through the values of its fields, and a runtime name there is excluded
// like a bare runtime name string. Reading it as any key's name collapses the
// Stripe gate to 0% (distilled's schema nodes copy a `key` field by computed
// key, and that field holds host values in the model).
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
