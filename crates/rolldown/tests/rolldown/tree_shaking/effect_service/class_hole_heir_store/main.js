import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Two objects inherit from a class's prototype without `new`: one from
// Object.create, one through a late Object.setPrototypeOf. Each is given a
// service in a field, which is a field of a class instance: both keys keep
// every method. Other is never stored there and still prunes.
const Created = Context.Service('test/ClassHoleHeirStoreCreated');
const Reparented = Context.Service('test/ClassHoleHeirStoreReparented');
const Other = Context.Service('test/ClassHoleHeirStoreOther');
class Base {}

const program = Effect.gen(function* () {
  const created = Object.create(Base.prototype);
  created.held = yield* Created;
  const reparented = {};
  Object.setPrototypeOf(reparented, Base.prototype);
  reparented.held = yield* Reparented;
  const other = yield* Other;
  return [
    created instanceof Base,
    reparented instanceof Base,
    created.held.createdUsed(),
    other.heirOtherUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(Created, Created.of({ createdUsed: () => 'c', createdUnread: () => 'x' })),
  Layer.succeed(
    Reparented,
    Reparented.of({ reparentedFirst: () => 'r', reparentedSecond: () => 's' }),
  ),
  Layer.succeed(Other, Other.of({ heirOtherUsed: () => 'o', heirOtherUnused: () => 'u' })),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  true,
  true,
  'c',
  'o',
]);
