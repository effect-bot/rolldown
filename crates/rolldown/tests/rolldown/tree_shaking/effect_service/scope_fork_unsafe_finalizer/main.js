import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Layer from 'effect/Layer';
import * as Scope from 'effect/Scope';

// A finalizer added to a child scope made with Scope.forkUnsafe calls a
// service method. One child is closed explicitly; the other is finalized by
// closing its parent. Each finalizer's method stays; each service's unread
// method is removed.
const ChildClose = Context.Service('test/ForkUnsafeChildClose');
const ParentClose = Context.Service('test/ForkUnsafeParentClose');

const layer = Layer.mergeAll(
  Layer.succeed(ChildClose)({ viaChildClose: () => 'child', childUnused: () => 'unused' }),
  Layer.succeed(ParentClose)({ viaParentClose: () => 'parent', parentUnused: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const childClose = yield* ChildClose;
  const parentClose = yield* ParentClose;
  const calls = [];

  const first = Scope.makeUnsafe();
  const child = Scope.forkUnsafe(first);
  yield* Scope.addFinalizer(
    child,
    Effect.sync(() => calls.push(childClose.viaChildClose())),
  );
  yield* Scope.close(child, Exit.void);

  const second = Scope.makeUnsafe();
  const nested = Scope.forkUnsafe(second);
  yield* Scope.addFinalizer(
    nested,
    Effect.sync(() => calls.push(parentClose.viaParentClose())),
  );
  yield* Scope.close(second, Exit.void);

  return calls;
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'child',
  'parent',
]);
