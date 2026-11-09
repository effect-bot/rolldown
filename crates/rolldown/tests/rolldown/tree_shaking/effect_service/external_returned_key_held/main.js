import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { hold, holdProto, take, takeProto, unbox } from './late.js';

// The external module returns keys it reached through ordinary values: one
// inside an object handed to it, one stored in an object it already holds
// after it was handed over, keys it inherits through a prototype (one given
// with the object, one installed after the handover). Each lookup through what
// it returns must reach the service's implementation, so the method read there
// must stay. Handing a key to unseen code bails nothing, so the unread methods
// still prune.
const Nested = Context.Service('test/ExternalReturnedNested');
const Held = Context.Service('test/ExternalReturnedHeld');
const Inherited = Context.Service('test/ExternalReturnedInherited');
const LiteralProto = Context.Service('test/ExternalReturnedLiteralProto');
const LateProto = Context.Service('test/ExternalReturnedLateProto');

const box = {};
hold(box);
box.inner = Held;

// The prototype is installed after the object was handed over.
const lateProto = {};
holdProto(lateProto);
Object.setPrototypeOf(lateProto, { inner: LateProto });

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [
    Context.get(context, unbox({ inner: Nested })).nestedUsed(),
    Context.get(context, take()).heldUsed(),
    Context.get(context, unbox(Object.create({ inner: Inherited }))).inheritedUsed(),
    Context.get(context, unbox({ __proto__: { inner: LiteralProto } })).literalProtoUsed(),
    Context.get(context, takeProto()).lateProtoUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(Nested, Nested.of({ nestedUsed: () => 'nested', nestedUnused: () => 'unused' })),
  Layer.succeed(Held, Held.of({ heldUsed: () => 'held', heldUnused: () => 'unused' })),
  Layer.succeed(
    Inherited,
    Inherited.of({ inheritedUsed: () => 'inherited', inheritedUnused: () => 'unused' }),
  ),
  Layer.succeed(
    LiteralProto,
    LiteralProto.of({ literalProtoUsed: () => 'literal', literalProtoUnused: () => 'unused' }),
  ),
  Layer.succeed(
    LateProto,
    LateProto.of({ lateProtoUsed: () => 'late', lateProtoUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'nested',
  'held',
  'inherited',
  'literal',
  'late',
]);
