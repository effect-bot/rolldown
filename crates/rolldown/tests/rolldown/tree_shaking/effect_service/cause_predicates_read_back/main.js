import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Cause's stable predicates read one property and call nothing: the brand
// checks isCause, isReason and isDone test a key on an unknown value, and
// isFailReason, isDieReason and hasFails read `_tag`. Each service reaches one
// predicate group and is then used directly or read back through the reason;
// that method stays and the service's unread method is removed.
const ForCause = Context.Service('test/PredicateIsCause');
const ForReason = Context.Service('test/PredicateIsReason');
const ForDone = Context.Service('test/PredicateIsDone');
const ForTag = Context.Service('test/PredicateTag');

const layer = Layer.mergeAll(
  Layer.succeed(ForCause)({ viaIsCause: () => 'cause', isCauseUnused: () => 'unused' }),
  Layer.succeed(ForReason)({ viaIsReason: () => 'reason', isReasonUnused: () => 'unused' }),
  Layer.succeed(ForDone)({ viaIsDone: () => 'done', isDoneUnused: () => 'unused' }),
  Layer.succeed(ForTag)({ viaFailReason: () => 'fail', tagUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const forCause = yield* ForCause;
  const forReason = yield* ForReason;
  const forDone = yield* ForDone;
  const forTag = yield* ForTag;
  const cause = Cause.fail(forTag);
  const tagged = [];
  for (const reason of cause.reasons) {
    tagged.push(Cause.isDieReason(reason));
    tagged.push(Cause.isFailReason(reason) ? reason.error.viaFailReason() : 'no');
  }
  return [
    Cause.isCause(forCause) ? 'yes' : forCause.viaIsCause(),
    Cause.isReason(forReason) ? 'yes' : forReason.viaIsReason(),
    Cause.isDone(forDone) ? 'yes' : forDone.viaIsDone(),
    Cause.hasFails(cause),
    tagged,
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'cause',
  'reason',
  'done',
  true,
  [false, 'fail'],
]);
