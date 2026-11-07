import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Logger from 'effect/Logger';
import * as References from 'effect/References';

// A service set as a log annotation and read back as an unknown value,
// narrowed before the call: with the key/value form through
// References.CurrentLogAnnotations, and with the record form by a custom
// logger through its fiber. Annotation values are `unknown` in Effect's public
// types, so a value read back holds no service even after narrowing (Tim's
// decision): the reads keep nothing and every method may go.
const Store = Context.Service('test/LogAnnotationReadBack');
const annotationUnusedHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store, {
  readReference: () => 'reference',
  readLogger: () => 'logger',
  annotationUnused: () => annotationUnusedHelper(),
});

const logged = [];
const logger = Logger.make(({ fiber }) => {
  const value = fiber.getRef(References.CurrentLogAnnotations).logged;
  if (
    typeof value === 'object' &&
    value !== null &&
    'readLogger' in value &&
    typeof value.readLogger === 'function'
  ) {
    logged.push(value.readLogger());
  }
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  const read = yield* Effect.gen(function* () {
    const value = (yield* References.CurrentLogAnnotations).held;
    if (
      typeof value === 'object' &&
      value !== null &&
      'readReference' in value &&
      typeof value.readReference === 'function'
    ) {
      return value.readReference();
    }
    return 'missing';
  }).pipe(Effect.annotateLogs('held', store));
  yield* Effect.log('read back').pipe(Effect.annotateLogs({ logged: store }));
  return [read, logged];
});

assert.deepStrictEqual(
  await Effect.runPromise(
    Effect.provide(program, Layer.mergeAll(StoreLayer, Logger.layer([logger]))),
  ),
  ['reference', ['logger']],
);
