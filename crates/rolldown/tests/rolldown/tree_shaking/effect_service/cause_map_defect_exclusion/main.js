import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';
// Cause.map never passes a defect to its callback. A Cause combines a typed
// error with a defect that is a service; the callback calls `mapped` on its
// input, and the defect is recovered separately and narrowed from unknown.
// The defect's `used` stays (soundness). Its `mapped` is never called, because
// only the typed error reaches the callback, so it is removed (precision), as
// is its unread method.
const Defect = Context.Service('test/MapDefect');
const layer = Layer.succeed(Defect)({
  used: () => 'used',
  mapped: () => 'defect mapped',
  defectUnused: () => 'unused',
});
const program = Effect.gen(function* () {
  const defect = yield* Defect;
  const error = { mapped: () => 'error mapped' };
  const mapped = Cause.map(Cause.combine(Cause.fail(error), Cause.die(defect)), (e) => e.mapped());
  const found = Cause.findError(mapped);
  const recovered = Cause.findDefect(mapped);
  let viaDefect = 'missing';
  if (Result.isSuccess(recovered)) {
    const value = recovered.success;
    if (
      typeof value === 'object' &&
      value !== null &&
      'used' in value &&
      typeof value.used === 'function'
    ) {
      viaDefect = value.used();
    }
  }
  return [Result.isSuccess(found) ? found.success : 'missing', viaDefect];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'error mapped',
  'used',
]);
