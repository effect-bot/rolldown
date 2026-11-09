import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Deferred from 'effect/Deferred';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';
// Values that a public parameter type admits must reach the parameter, whatever
// shape the analysis sees: a service passed as Option.match's handlers object,
// a handlers object whose `onSome` comes from its prototype, a handlers object
// whose `onSome` is replaced by a store in a function that runs later, and an
// Effect passed where Deferred.complete expects an Effect. Each read method
// stays; each service's unread method is removed, except Inherited's: the class
// whose prototype supplies `onSome` closes over that service, so it reaches class
// code and its key keeps every method. Handlers' unread method may stay too: the
// service is the handlers object, and an options object's members all count as
// read.
const Handlers = Context.Service('test/BoundHandlers');
const Inherited = Context.Service('test/BoundInherited');
const Late = Context.Service('test/BoundLate');
const Completed = Context.Service('test/BoundDeferred');
const layer = Layer.mergeAll(
  Layer.succeed(Handlers)({
    onNone: () => 'none',
    onSome: (n) => `some ${n}`,
    handlersUnused: () => 'unused',
  }),
  Layer.succeed(Inherited)({ viaInherited: () => 'inherited', inheritedUnused: () => 'unused' }),
  Layer.succeed(Late)({ viaLate: () => 'late', lateUnused: () => 'unused' }),
  Layer.succeed(Completed)({ viaDeferred: () => 'deferred', deferredUnused: () => 'unused' }),
);
const program = Effect.gen(function* () {
  const handlers = yield* Handlers;
  const inherited = yield* Inherited;
  const late = yield* Late;
  const completed = yield* Completed;
  // `onSome` lives on the prototype of the handlers object.
  class ProtoHandlers {
    onNone() {
      return 'none';
    }
    onSome() {
      return inherited.viaInherited();
    }
  }
  // `onSome` is replaced by a store in a function that runs before the match.
  const lateHandlers = { onNone: () => 'none', onSome: (_n) => 'placeholder' };
  yield* Effect.sync(() => {
    lateHandlers.onSome = () => late.viaLate();
  });
  const deferred = yield* Deferred.make();
  yield* Deferred.complete(
    deferred,
    Effect.sync(() => completed.viaDeferred()),
  );
  return [
    Option.match(Option.some(1), handlers),
    Option.match(Option.some(2), new ProtoHandlers()),
    Option.match(Option.some(3), lateHandlers),
    yield* Deferred.await(deferred),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'some 1',
  'inherited',
  'late',
  'deferred',
]);
