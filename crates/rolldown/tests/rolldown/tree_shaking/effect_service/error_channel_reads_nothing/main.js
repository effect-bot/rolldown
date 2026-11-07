import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Fiber from 'effect/Fiber';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';
const Caught = Context.Service('test/ErrorChannelCaught');
const Defect = Context.Service('test/ErrorChannelDefect');
const Thrown = Context.Service('test/ErrorChannelThrown');
const Joined = Context.Service('test/ErrorChannelJoined');
const Exited = Context.Service('test/ErrorChannelExited');
const Mapped = Context.Service('test/ErrorChannelMapped');
const Held = Context.Service('test/ErrorChannelContext');
const Direct = Context.Service('test/ErrorChannelDirect');
const layer = Layer.mergeAll(
  Layer.succeed(Caught)({ viaCatch: () => 'caught', catchUnused: () => 'unused' }),
  Layer.succeed(Defect)({ viaDefect: () => 'defect', defectUnused: () => 'unused' }),
  Layer.succeed(Thrown)({ viaTry: () => 'thrown', tryUnused: () => 'unused' }),
  Layer.succeed(Joined)({ viaJoin: () => 'joined', joinUnused: () => 'unused' }),
  Layer.succeed(Exited)({ viaExitCause: () => 'exited', exitUnused: () => 'unused' }),
  Layer.succeed(Mapped)({ viaMapInput: () => 'mapped', mapUnused: () => 'unused' }),
  Layer.succeed(Held)({ viaFailureContext: () => 'context', contextUnused: () => 'unused' }),
  Layer.succeed(Direct)({ viaDirect: () => 'direct', directUnused: () => 'unused' }),
);
const program = Effect.gen(function* () {
  const caught = yield* Caught;
  const defect = yield* Defect;
  const thrown = yield* Thrown;
  const joined = yield* Joined;
  const exited = yield* Exited;
  const mapped = yield* Mapped;
  const direct = yield* Direct;
  const viaCatch = yield* Effect.fail(caught).pipe(
    Effect.catch((error) => Effect.succeed(error.viaCatch())),
  );
  const viaDefect = yield* Effect.die(defect).pipe(
    Effect.catchDefect((value) =>
      Effect.succeed(
        typeof value === 'object' &&
          value !== null &&
          'viaDefect' in value &&
          typeof value.viaDefect === 'function'
          ? value.viaDefect()
          : 'missing',
      ),
    ),
  );
  const viaTry = yield* Effect.try({
    try: () => {
      throw thrown;
    },
    catch: (error) => error,
  }).pipe(
    Effect.catch((error) =>
      Effect.succeed(
        typeof error === 'object' &&
          error !== null &&
          'viaTry' in error &&
          typeof error.viaTry === 'function'
          ? error.viaTry()
          : 'missing',
      ),
    ),
  );
  const fiber = yield* Effect.forkChild(Effect.fail(joined));
  const viaJoin = yield* Effect.catch(Fiber.join(fiber), (error) =>
    Effect.succeed(error.viaJoin()),
  );
  const exit = yield* Effect.exit(Effect.fail(exited));
  let viaExitCause = 'missing';
  if (Exit.isFailure(exit)) {
    const found = Cause.findError(exit.cause);
    if (Result.isSuccess(found)) viaExitCause = found.success.viaExitCause();
  }
  const fromMap = Cause.findError(Cause.map(Cause.fail(mapped), (error) => error.viaMapInput()));
  const context = yield* Effect.context();
  const viaFailureContext = yield* Effect.fail(context).pipe(
    Effect.catch((error) => Effect.succeed(Context.get(error, Held).viaFailureContext())),
  );
  return [
    viaCatch,
    viaDefect,
    viaTry,
    viaJoin,
    viaExitCause,
    Result.isSuccess(fromMap) ? fromMap.success : 'missing',
    viaFailureContext,
    direct.viaDirect(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'caught',
  'defect',
  'thrown',
  'joined',
  'exited',
  'mapped',
  'context',
  'direct',
]);
