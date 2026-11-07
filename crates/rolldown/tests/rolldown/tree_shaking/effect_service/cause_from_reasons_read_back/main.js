import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';

// Cause.fromReasons builds a Cause from another Cause's reasons; its error is
// read back with Cause.findError, narrowed with Result.isSuccess. Once the
// service is the error itself, once it sits in a nested field of the error.
// Each read method stays; each service's unread method is removed.
const Direct = Context.Service('test/FromReasonsDirect');
const Nested = Context.Service('test/FromReasonsNested');

const layer = Layer.mergeAll(
  Layer.succeed(Direct)({ viaFromReasons: () => 'direct', directUnused: () => 'unused' }),
  Layer.succeed(Nested)({ viaNestedReasons: () => 'nested', nestedUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const direct = yield* Direct;
  const nested = yield* Nested;
  const fromDirect = Cause.findError(Cause.fromReasons(Cause.fail(direct).reasons));
  const fromNested = Cause.findError(
    Cause.fromReasons(Cause.fail({ inner: { service: nested } }).reasons),
  );
  return [
    Result.isSuccess(fromDirect) ? fromDirect.success.viaFromReasons() : 'missing',
    Result.isSuccess(fromNested) ? fromNested.success.inner.service.viaNestedReasons() : 'missing',
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'direct',
  'nested',
]);
