import assert from 'node:assert';
import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Result from 'effect/Result';
// Cause.map calls its callback with each typed error and makes the callback's
// returns the new errors. Each case has its own service: the callback's input
// (data-last form), a service the callback allocates and returns, a nested
// field and a bound function in the callback's return, and a service reached
// through annotations that Effect merges for a mapped error object it has seen
// before. Each read method stays; each unread method is removed.
const Input = Context.Service('test/MapInput');
const Made = Context.Service('test/MapMade');
const Nested = Context.Service('test/MapNested');
const Bound = Context.Service('test/MapBound');
const Annotated = Context.Service('test/MapAnnotated');
const AnnotationKey = Context.Service('test/MapAnnotationKey');
const OtherKey = Context.Service('test/MapOtherKey');
// The service the Made layer provides is allocated by the callback.
const makeService = () => ({ discovered: () => 'made', madeUnused: () => 'unused' });
const MadeLayer = Layer.effect(
  Made,
  Effect.suspend(() => {
    const found = Cause.findError(Cause.map(Cause.fail(0), () => makeService()));
    return Result.isSuccess(found) ? Effect.succeed(found.success) : Effect.die('missing');
  }),
);
const layer = Layer.mergeAll(
  Layer.succeed(Input)({ mapped: () => 'input', inputUnused: () => 'unused' }),
  MadeLayer,
  Layer.succeed(Nested)({ viaNestedMap: () => 'nested', nestedUnused: () => 'unused' }),
  Layer.succeed(Bound)({ viaRun: () => 'bound', boundUnused: () => 'unused' }),
  Layer.succeed(Annotated)({ viaAnnotation: () => 'annotated', annotatedUnused: () => 'unused' }),
);
function readBound() {
  return this.viaRun();
}
const program = Effect.gen(function* () {
  const input = yield* Input;
  const made = yield* Made;
  const nested = yield* Nested;
  const bound = yield* Bound;
  const annotated = yield* Annotated;
  const fromInput = Cause.findError(Cause.fail(input).pipe(Cause.map((e) => e.mapped())));
  const members = Cause.findError(
    Cause.map(Cause.fail(0), () => ({ inner: { service: nested }, run: readBound.bind(bound) })),
  );
  // The first map associates the annotations with `shared`; the second map of
  // a differently annotated failure to the same object exposes both.
  const shared = { label: 'shared' };
  Cause.map(
    Cause.fail(1).pipe(Cause.annotate(Context.make(AnnotationKey, annotated))),
    () => shared,
  );
  const second = Cause.map(
    Cause.fail(2).pipe(Cause.annotate(Context.make(OtherKey, 'other'))),
    () => shared,
  );
  let fromAnnotation = 'missing';
  for (const reason of second.reasons) {
    const value = reason.annotations.get('test/MapAnnotationKey');
    if (
      typeof value === 'object' &&
      value !== null &&
      'viaAnnotation' in value &&
      typeof value.viaAnnotation === 'function'
    ) {
      fromAnnotation = value.viaAnnotation();
    }
  }
  return [
    Result.isSuccess(fromInput) ? fromInput.success : 'missing',
    made.discovered(),
    Result.isSuccess(members) ? members.success.inner.service.viaNestedMap() : 'missing',
    Result.isSuccess(members) ? members.success.run() : 'missing',
    fromAnnotation,
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'input',
  'made',
  'nested',
  'bound',
  'annotated',
]);
