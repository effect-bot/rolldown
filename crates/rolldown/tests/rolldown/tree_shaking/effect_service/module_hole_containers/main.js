import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { build } from './built.js';
import { passThrough as layerThrough } from './layer.js';
import { entriesFrom } from './made.js';
import { entries } from './mapped.js';
import { passThrough as omitThrough } from './omitted.js';
import { callWith, passThrough } from './passed.js';

// Contexts and Layers that go into a module that never imports `effect` and
// come back out, contexts its callbacks build, and Context.makeUnsafe over maps
// it builds. Every called method stays.
const Passed = Context.Service('test/HoleContainersPassed');
const passed = Context.make(Passed, {
  passedArg: () => 'arg',
  passedReturn: () => 'return',
  passedUnused: () => 'unused',
});
assert.strictEqual(
  callWith(passed, (ctx) => Context.get(ctx, Passed).passedArg()),
  'arg',
);
assert.strictEqual(Context.get(passThrough(passed), Passed).passedReturn(), 'return');

const Layered = Context.Service('test/HoleContainersLayer');
const layer = layerThrough(
  Layer.succeed(Layered, { layerUsed: () => 'layer', layerUnused: () => 'unused' }),
);
assert.strictEqual(
  Effect.runSync(
    Effect.provide(
      Effect.gen(function* () {
        return (yield* Layered).layerUsed();
      }),
      layer,
    ),
  ),
  'layer',
);

const Built = Context.Service('test/HoleContainersBuilt');
const built = build(() =>
  Context.make(Built, { builtUsed: () => 'built', builtUnused: () => 'unused' }),
);
assert.strictEqual(Context.get(built, Built).builtUsed(), 'built');

const Mapped = Context.Service('test/HoleContainersMapped');
const mapped = Context.makeUnsafe(
  entries('test/HoleContainersMapped', {
    mappedUsed: () => 'mapped',
    mappedUnused: () => 'unused',
  }),
);
assert.strictEqual(Context.get(mapped, Mapped).mappedUsed(), 'mapped');

const Made = Context.Service('test/HoleContainersMade');
const made = Context.makeUnsafe(
  entriesFrom('test/HoleContainersMade', () => ({
    madeUsed: () => 'made',
    madeUnused: () => 'unused',
  })),
);
assert.strictEqual(Context.get(made, Made).madeUsed(), 'made');

// A Context from a module hole merged into one the pass builds, then handed to
// an API it has no rule for: what that API hands back is looked up through the
// merged Context's keys, so the hole's keys have to be among them.
const Omitted = Context.Service('test/HoleContainersOmitted');
const Other = Context.Service('test/HoleContainersOther');
const omitted = Context.addOrOmit(
  Context.merge(
    omitThrough(
      Context.make(Omitted, { omittedUsed: () => 'omitted', omittedUnused: () => 'unused' }),
    ),
    Context.empty(),
  ),
  Other,
  { _tag: 'None' },
);
const fromOmitted = [];
for (const [, service] of omitted.mapUnsafe) fromOmitted.push(service.omittedUsed());
assert.deepStrictEqual(fromOmitted, ['omitted']);
