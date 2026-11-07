import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Errors built with Cause.fail and recovered with Effect.catch reach a service
// through their members, one way per service: a nested data field, a data field
// a method reads through `this`, a bound function, and a builtin (String) that
// calls the service's toString. Each path's method stays; each service's
// unread method is removed.
const Nested = Context.Service('test/CauseMemberNested');
const Receiver = Context.Service('test/CauseMemberReceiver');
const Bound = Context.Service('test/CauseMemberBound');
const Builtin = Context.Service('test/CauseMemberBuiltin');

const layer = Layer.mergeAll(
  Layer.succeed(Nested)({ viaNested: () => 'nested', nestedUnused: () => 'unused' }),
  Layer.succeed(Receiver)({ viaReceiver: () => 'receiver', receiverUnused: () => 'unused' }),
  Layer.succeed(Bound)({ viaBound: () => 'bound', boundUnused: () => 'unused' }),
  Layer.succeed(Builtin)({
    toString() {
      return 'S:' + this.stringHelper();
    },
    stringHelper: () => 'string',
    builtinUnused: () => 'unused',
  }),
);

function readBound() {
  return this.viaBound();
}

const program = Effect.gen(function* () {
  const nested = yield* Nested;
  const receiver = yield* Receiver;
  const bound = yield* Bound;
  const builtin = yield* Builtin;
  return [
    yield* Effect.failCause(Cause.fail({ inner: { service: nested } })).pipe(
      Effect.catch((error) => Effect.succeed(error.inner.service.viaNested())),
    ),
    yield* Effect.failCause(
      Cause.fail({
        value: receiver,
        read() {
          return this.value.viaReceiver();
        },
      }),
    ).pipe(Effect.catch((error) => Effect.succeed(error.read()))),
    yield* Effect.failCause(Cause.fail({ read: readBound.bind(bound) })).pipe(
      Effect.catch((error) => Effect.succeed(error.read())),
    ),
    yield* Effect.failCause(Cause.fail({ convert: String })).pipe(
      Effect.catch((error) => Effect.succeed(error.convert(builtin))),
    ),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'nested',
  'receiver',
  'bound',
  'S:string',
]);
