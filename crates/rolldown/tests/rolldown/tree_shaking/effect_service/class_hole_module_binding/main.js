import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class method calls a method of a module-level implementation literal that
// is also provided under Store. The class body references the service itself,
// so Store keeps every method. Other still prunes.
const Store = Context.Service('test/ClassHoleModuleBindingStore');
const Other = Context.Service('test/ClassHoleModuleBindingOther');
const impl = Store.of({
  bindingUsed: () => 'used',
  bindingUnread: () => 'unread',
});
class Reader {
  read() {
    return impl.bindingUsed();
  }
}

const program = Effect.gen(function* () {
  yield* Store;
  const other = yield* Other;
  return [new Reader().read(), other.bindingOtherUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(Store, impl),
  Layer.succeed(
    Other,
    Other.of({
      bindingOtherUsed: () => 'other',
      bindingOtherUnused: () => 'unused',
    }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['used', 'other']);
