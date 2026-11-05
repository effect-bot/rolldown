import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// Control: run A hands a plain object to code compiled from text built at run
// time, which turns one of its fields into a getter. Run B stores its service
// in that object and reads the field statically, so the getter calls a method
// of run B's service chosen at run time. Run B never calls the compiled code
// itself, yet its service keeps every method.
const Store = Context.Service('test/RealOpaqueAccessorStore');
const Other = Context.Service('test/RealOpaqueAccessorOther');
const StoreLayer = Layer.succeed(Store, Store.of({ readStore: () => 's1' }));
const accessorOtherUnusedHelper = () => 'o2';
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => 'o1';
    const accessorOtherUnused = () => accessorOtherUnusedHelper();
    return Other.of({ readOther, accessorOtherUnused });
  }),
);

const registry = { label: 'plain', service: undefined };
const source = [
  "Object.defineProperty(registry, 'label', { get() {",
  " return this.service['accessorOther' + 'Unused']() } })",
].join('');
const runA = Effect.gen(function* () {
  Function('registry', source)(registry);
  return (yield* Store).readStore();
});
const runB = Effect.gen(function* () {
  const other = yield* Other;
  registry.service = other;
  return [other.readOther(), registry.label];
});

assert.strictEqual(await Effect.runPromise(Effect.provide(runA, StoreLayer)), 's1');
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(runB, OtherLayer)), ['o1', 'o2']);
