import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';
const OnCatch = Context.Service('test/CaptureCatch');
const OnDefect = Context.Service('test/CaptureDefect');
const OnMapError = Context.Service('test/CaptureMapError');
const OnTapError = Context.Service('test/CaptureTapError');
const OnExit = Context.Service('test/CaptureExit');
const OnCauseMap = Context.Service('test/CaptureCauseMap');
const OnThrow = Context.Service('test/CapturePlainThrow');
const layer = Layer.mergeAll(
  Layer.succeed(OnCatch)({ viaCatchHandler: () => 'catch', catchHandlerUnused: () => 'unused' }),
  Layer.succeed(OnDefect)({
    viaDefectHandler: () => 'defect',
    defectHandlerUnused: () => 'unused',
  }),
  Layer.succeed(OnMapError)({ viaMapError: () => 'mapError', mapErrorUnused: () => 'unused' }),
  Layer.succeed(OnTapError)({ viaTapError: () => 'tapError', tapErrorUnused: () => 'unused' }),
  Layer.succeed(OnExit)({ viaExitFailure: () => 'exit', exitFailureUnused: () => 'unused' }),
  Layer.succeed(OnCauseMap)({ viaCauseMap: () => 'causeMap', causeMapUnused: () => 'unused' }),
  Layer.succeed(OnThrow)({ viaPlainCatch: () => 'plain', plainCatchUnused: () => 'unused' }),
);
const program = Effect.gen(function* () {
  const onCatch = yield* OnCatch;
  const onDefect = yield* OnDefect;
  const onMapError = yield* OnMapError;
  const onTapError = yield* OnTapError;
  const onExit = yield* OnExit;
  const onCauseMap = yield* OnCauseMap;
  const onThrow = yield* OnThrow;
  const tapped = [];
  const caught = yield* Effect.fail('failed').pipe(
    Effect.catch(() => Effect.succeed(onCatch.viaCatchHandler())),
  );
  const defect = yield* Effect.die('died').pipe(
    Effect.catchDefect(() => Effect.succeed(onDefect.viaDefectHandler())),
  );
  const mappedError = yield* Effect.fail('failed').pipe(
    Effect.mapError(() => onMapError.viaMapError()),
    Effect.catch((error) => Effect.succeed(error)),
  );
  yield* Effect.fail('failed').pipe(
    Effect.tapError(() => Effect.sync(() => tapped.push(onTapError.viaTapError()))),
    Effect.catch(() => Effect.void),
  );
  const exit = yield* Effect.exit(Effect.fail('failed'));
  const exitFailure = Exit.match(exit, {
    onFailure: () => onExit.viaExitFailure(),
    onSuccess: () => 'success',
  });
  const causeMapped = Cause.findError(
    Cause.map(Cause.fail('failed'), () => onCauseMap.viaCauseMap()),
  );
  let plain = 'missing';
  try {
    throw onThrow;
  } catch (error) {
    if (
      typeof error === 'object' &&
      error !== null &&
      'viaPlainCatch' in error &&
      typeof error.viaPlainCatch === 'function'
    ) {
      plain = error.viaPlainCatch();
    }
  }
  return [
    caught,
    defect,
    mappedError,
    tapped,
    exitFailure,
    Result.isSuccess(causeMapped) ? causeMapped.success : 'missing',
    plain,
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'catch',
  'defect',
  'mapError',
  ['tapError'],
  'exit',
  'causeMap',
  'plain',
]);
