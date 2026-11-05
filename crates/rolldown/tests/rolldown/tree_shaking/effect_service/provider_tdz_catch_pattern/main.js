import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A provider property reads a destructured catch binding inside that
// binding's own default, in an object and an array pattern. The binding is
// still in its temporal dead zone, so building the provider throws. Removing
// the unread property would drop that exception.
const ObjectPattern = Context.Service('test/RealTdzCatchObjectPattern');
const ArrayPattern = Context.Service('test/RealTdzCatchArrayPattern');

const readObjectPattern = Effect.gen(function* () {
  return (yield* ObjectPattern).readOne();
});
const readArrayPattern = Effect.gen(function* () {
  return (yield* ArrayPattern).readOne();
});

const throwsReferenceError = (run) => {
  try {
    run();
  } catch (error) {
    if (!(error instanceof ReferenceError)) throw error;
    return true;
  }
  return false;
};

assert.strictEqual(
  throwsReferenceError(() => {
    try {
      throw {};
    } catch ({
      objectHelper = Effect.runSync(
        Effect.provide(
          readObjectPattern,
          Layer.succeed(ObjectPattern, { readOne: () => 'read', tdzUnused: objectHelper }),
        ),
      ),
    }) {}
  }),
  true,
);

assert.strictEqual(
  throwsReferenceError(() => {
    try {
      throw [];
    } catch ([
      arrayHelper = Effect.runSync(
        Effect.provide(
          readArrayPattern,
          Layer.succeed(ArrayPattern, { readOne: () => 'read', tdzUnused: arrayHelper }),
        ),
      ),
    ]) {}
  }),
  true,
);
