import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Cause's brand checks isCause, isReason and isDone test a key on an unknown
// value and call nothing. Each service is passed to one of them and then used
// directly; that method stays and the service's unread method is removed. (No
// service goes into a Cause: services never enter the error channel.)
const ForCause = Context.Service('test/PredicateIsCause');
const ForReason = Context.Service('test/PredicateIsReason');
const ForDone = Context.Service('test/PredicateIsDone');

const layer = Layer.mergeAll(
  Layer.succeed(ForCause)({ viaIsCause: () => 'cause', isCauseUnused: () => 'unused' }),
  Layer.succeed(ForReason)({ viaIsReason: () => 'reason', isReasonUnused: () => 'unused' }),
  Layer.succeed(ForDone)({ viaIsDone: () => 'done', isDoneUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const forCause = yield* ForCause;
  const forReason = yield* ForReason;
  const forDone = yield* ForDone;
  return [
    Cause.isCause(forCause) ? 'yes' : forCause.viaIsCause(),
    Cause.isReason(forReason) ? 'yes' : forReason.viaIsReason(),
    Cause.isDone(forDone) ? 'yes' : forDone.viaIsDone(),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'cause',
  'reason',
  'done',
]);
