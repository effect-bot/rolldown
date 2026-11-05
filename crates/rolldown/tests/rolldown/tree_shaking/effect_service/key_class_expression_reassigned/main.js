import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: the `var` holding the class-expression key is reassigned to
// another key at run time, so it does not name one key. Every method stays.
var Store = class extends Context.Service()('test/RealClassExpressionReassignedA') {};
const Other = Context.Service('test/RealClassExpressionReassignedB');
const reassignedOtherUnusedHelper = () => 'reassignedOtherUnused';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'readOther';
    const reassignedOtherUnused = () => reassignedOtherUnusedHelper();
    return Other.of({ readOther, reassignedOtherUnused });
  }),
);

if (Number('1') === 1) Store = Other;

const program = Effect.gen(function* () {
  return (yield* Store).reassignedOtherUnused();
});

assert.strictEqual(
  await Effect.runPromise(Effect.provide(program, OtherLayer)),
  'reassignedOtherUnused',
);
