import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Methods that read their receiver in ways the body alone does not show.
const DefaultParam = Context.Service('test/DefaultParam');
const Reassigned = Context.Service('test/Reassigned');

function read() {
  return 1;
}
read = function () {
  return this.receiverReassignedBase();
};

const DefaultParamLayer = Layer.effect(
  DefaultParam,
  Effect.gen(function* () {
    return DefaultParam.of({
      read: function (value = this.receiverDefaultBase()) {
        return value;
      },
      receiverDefaultBase: () => 2,
      receiverDefaultUnused: () => 5,
    });
  }),
);
const ReassignedLayer = Layer.effect(
  Reassigned,
  Effect.gen(function* () {
    return { read, receiverReassignedBase: () => 2, receiverReassignedUnused: () => 6 };
  }),
);

const program = Effect.gen(function* () {
  return [(yield* DefaultParam).read(), (yield* Reassigned).read()];
});

assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(program, Layer.mergeAll(DefaultParamLayer, ReassignedLayer)),
  ),
  [2, 2],
);
