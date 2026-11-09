import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// The fixture sets the widening limit to 8, so the elements of each
// eleven-value array below are widened: read at a computed index, they are the
// opaque boundary's value. A key-shaped value that crosses the boundary may
// come back as itself, so every lookup through such an element must reach the
// service's implementation, and the method read there must stay. The key
// properties are written after the arrays are built, so the key's name
// reaches the widened value late.
const LateObject = Context.Service('test/WidenedLateObject');
const LateElement = Context.Service('test/WidenedLateElement');
const LateFunction = Context.Service('test/WidenedLateFunction');
const Provided = Context.Service('test/WidenedLateProvided');

const last = (array) => array.length - 1;

// An object given its key property after it was widened.
const objectKey = {};
const objects = [
  { key: 'test/WidenedObject0' },
  { key: 'test/WidenedObject1' },
  { key: 'test/WidenedObject2' },
  { key: 'test/WidenedObject3' },
  { key: 'test/WidenedObject4' },
  { key: 'test/WidenedObject5' },
  { key: 'test/WidenedObject6' },
  { key: 'test/WidenedObject7' },
  { key: 'test/WidenedObject8' },
  { key: 'test/WidenedObject9' },
  objectKey,
];
objectKey.key = 'test/WidenedLateObject';

// The key property written through the widened element itself.
const elements = [
  { key: 'test/WidenedElement0' },
  { key: 'test/WidenedElement1' },
  { key: 'test/WidenedElement2' },
  { key: 'test/WidenedElement3' },
  { key: 'test/WidenedElement4' },
  { key: 'test/WidenedElement5' },
  { key: 'test/WidenedElement6' },
  { key: 'test/WidenedElement7' },
  { key: 'test/WidenedElement8' },
  { key: 'test/WidenedElement9' },
  {},
];
elements[last(elements)].key = 'test/WidenedLateElement';

// A function given its key property late, among functions with key properties.
const functionKey = function () {};
const functions = [
  Object.assign(() => 0, { key: 'test/WidenedFunction0' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction1' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction2' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction3' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction4' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction5' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction6' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction7' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction8' }),
  Object.assign(() => 0, { key: 'test/WidenedFunction9' }),
  functionKey,
];
functionKey.key = 'test/WidenedLateFunction';

// A service provided under a widened hand-made key and looked up through its
// class.
const provided = [
  { key: 'test/WidenedProvided0' },
  { key: 'test/WidenedProvided1' },
  { key: 'test/WidenedProvided2' },
  { key: 'test/WidenedProvided3' },
  { key: 'test/WidenedProvided4' },
  { key: 'test/WidenedProvided5' },
  { key: 'test/WidenedProvided6' },
  { key: 'test/WidenedProvided7' },
  { key: 'test/WidenedProvided8' },
  { key: 'test/WidenedProvided9' },
  { key: 'test/WidenedLateProvided' },
];

const program = Effect.gen(function* () {
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context));
  const fromProvided = yield* Provided;
  return [
    Context.get(context, objects[last(objects)]).lateObjectUsed(),
    Context.get(context, elements[last(elements)]).lateElementUsed(),
    Context.get(context, functions[last(functions)]).lateFunctionUsed(),
    fromProvided.providedUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    LateObject,
    LateObject.of({ lateObjectUsed: () => 'object', lateObjectUnused: () => 'unused' }),
  ),
  Layer.succeed(
    LateElement,
    LateElement.of({ lateElementUsed: () => 'element', lateElementUnused: () => 'unused' }),
  ),
  Layer.succeed(
    LateFunction,
    LateFunction.of({ lateFunctionUsed: () => 'function', lateFunctionUnused: () => 'unused' }),
  ),
  Layer.succeed(provided[last(provided)], {
    providedUsed: () => 'provided',
    providedUnused: () => 'unused',
  }),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'object',
  'element',
  'function',
  'provided',
]);
