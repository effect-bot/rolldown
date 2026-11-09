import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The fixture sets the widening limit to 8, so each eleven-name array below is
// widened: read at a computed index, an element is the opaque boundary's value.
// One name in each array is only known at run time (the test sets the
// variables to the services' key names), so it may name any key: a lookup by
// the widened element, or by a key-shaped object holding that name, must still
// reach the service's implementation, whether the runtime name arrived before
// the array was widened or after.
const Early = Context.Service('test/WidenedRuntimeEarly');
const Late = Context.Service('test/WidenedRuntimeLate');
const ObjectEarly = Context.Service('test/WidenedRuntimeObjectEarly');
const ObjectLate = Context.Service('test/WidenedRuntimeObjectLate');

const last = (array) => array.length - 1;

// The runtime name comes first, before the literal names widen the array.
const early = [
  process.env.WIDENED_EARLY_NAME,
  'test/WidenedNameEarly0',
  'test/WidenedNameEarly1',
  'test/WidenedNameEarly2',
  'test/WidenedNameEarly3',
  'test/WidenedNameEarly4',
  'test/WidenedNameEarly5',
  'test/WidenedNameEarly6',
  'test/WidenedNameEarly7',
  'test/WidenedNameEarly8',
  'test/WidenedNameEarly9',
];
const earlyName = early[0];
early[0] = early[last(early)];
early[last(early)] = earlyName;

// The runtime name is written after the literal names widened the array.
const late = [
  'test/WidenedNameLate0',
  'test/WidenedNameLate1',
  'test/WidenedNameLate2',
  'test/WidenedNameLate3',
  'test/WidenedNameLate4',
  'test/WidenedNameLate5',
  'test/WidenedNameLate6',
  'test/WidenedNameLate7',
  'test/WidenedNameLate8',
  'test/WidenedNameLate9',
  'test/WidenedNameLatePlaceholder',
];
late[last(late)] = process.env.WIDENED_LATE_NAME;

// Key-shaped objects: one built with the runtime name before the literal keys
// widen the array, and one given it through the widened element afterwards.
const objectsEarly = [
  { key: process.env.WIDENED_OBJECT_EARLY_NAME },
  { key: 'test/WidenedKeyEarly0' },
  { key: 'test/WidenedKeyEarly1' },
  { key: 'test/WidenedKeyEarly2' },
  { key: 'test/WidenedKeyEarly3' },
  { key: 'test/WidenedKeyEarly4' },
  { key: 'test/WidenedKeyEarly5' },
  { key: 'test/WidenedKeyEarly6' },
  { key: 'test/WidenedKeyEarly7' },
  { key: 'test/WidenedKeyEarly8' },
  { key: 'test/WidenedKeyEarly9' },
];
const objectEarly = objectsEarly[0];
objectsEarly[0] = objectsEarly[last(objectsEarly)];
objectsEarly[last(objectsEarly)] = objectEarly;

const objectsLate = [
  { key: 'test/WidenedKeyLate0' },
  { key: 'test/WidenedKeyLate1' },
  { key: 'test/WidenedKeyLate2' },
  { key: 'test/WidenedKeyLate3' },
  { key: 'test/WidenedKeyLate4' },
  { key: 'test/WidenedKeyLate5' },
  { key: 'test/WidenedKeyLate6' },
  { key: 'test/WidenedKeyLate7' },
  { key: 'test/WidenedKeyLate8' },
  { key: 'test/WidenedKeyLate9' },
  { key: 'test/WidenedKeyLatePlaceholder' },
];
objectsLate[last(objectsLate)].key = process.env.WIDENED_OBJECT_LATE_NAME;

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  return [
    Context.getOrUndefinedUnsafe(context, early[last(early)]).earlyUsed(),
    Context.getOrUndefinedUnsafe(context, late[last(late)]).lateUsed(),
    Context.get(context, objectsEarly[last(objectsEarly)]).objectEarlyUsed(),
    Context.get(context, objectsLate[last(objectsLate)]).objectLateUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(Early, Early.of({ earlyUsed: () => 'early', earlyUnused: () => 'unused' })),
  Layer.succeed(Late, Late.of({ lateUsed: () => 'late', lateUnused: () => 'unused' })),
  Layer.succeed(
    ObjectEarly,
    ObjectEarly.of({ objectEarlyUsed: () => 'object early', objectEarlyUnused: () => 'unused' }),
  ),
  Layer.succeed(
    ObjectLate,
    ObjectLate.of({ objectLateUsed: () => 'object late', objectLateUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'early',
  'late',
  'object early',
  'object late',
]);
