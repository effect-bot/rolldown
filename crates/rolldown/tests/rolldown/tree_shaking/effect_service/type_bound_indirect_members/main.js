import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Option from 'effect/Option';
// A handlers object satisfies Option.match's parameter type through members the
// analysis sees indirectly: a getter installed with Object.defineProperty, a class
// accessor, a prototype filled by Object.assign, and a store under a computed key
// after the call is built. Each object must still reach Option.match, so the
// method its onSome reads stays; each service's unread method is removed.
const Getter = Context.Service('test/MemberGetter');
const Accessor = Context.Service('test/MemberAccessor');
const Assigned = Context.Service('test/MemberAssigned');
const Stored = Context.Service('test/MemberStored');
const layer = Layer.mergeAll(
  Layer.succeed(Getter)({ viaGetter: () => 'getter', getterUnused: () => 'unused' }),
  Layer.succeed(Accessor)({ viaAccessor: () => 'accessor', accessorUnused: () => 'unused' }),
  Layer.succeed(Assigned)({ viaAssigned: () => 'assigned', assignedUnused: () => 'unused' }),
  Layer.succeed(Stored)({ viaStored: () => 'stored', storedUnused: () => 'unused' }),
);
const program = Effect.gen(function* () {
  const getter = yield* Getter;
  const accessor = yield* Accessor;
  const assigned = yield* Assigned;
  const stored = yield* Stored;
  const viaGetter = { onNone: () => 'none' };
  Object.defineProperty(viaGetter, 'onSome', { get: () => () => getter.viaGetter() });
  class ViaAccessor {
    onNone() {
      return 'none';
    }
    get onSome() {
      return () => accessor.viaAccessor();
    }
  }
  const proto = {};
  Object.assign(proto, { onSome: () => assigned.viaAssigned() });
  const viaAssigned = Object.create(proto);
  viaAssigned.onNone = () => 'none';
  const viaStored = { onNone: () => 'none' };
  const matchStored = () => Option.match(Option.some(4), viaStored);
  viaStored['onSomeX'.slice(0, 6)] = () => stored.viaStored();
  return [
    Option.match(Option.some(1), viaGetter),
    Option.match(Option.some(2), new ViaAccessor()),
    Option.match(Option.some(3), viaAssigned),
    matchStored(),
  ];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  'getter',
  'accessor',
  'assigned',
  'stored',
]);
