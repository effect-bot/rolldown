import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control for the catch-binding check: a destructured catch binding read in
// the catch body is initialised, so the unread property can go.
const CatchBody = Context.Service('test/RealInitializedCatchBody');

const readCatchBody = Effect.gen(function* () {
  return (yield* CatchBody).readOne();
});

let result;
try {
  throw { catchHelper: () => 'unused' };
} catch ({ catchHelper }) {
  result = Effect.runSync(
    Effect.provide(
      readCatchBody,
      Layer.succeed(CatchBody, { readOne: () => 'read', catchUnused: catchHelper }),
    ),
  );
}
assert.strictEqual(result, 'read');
