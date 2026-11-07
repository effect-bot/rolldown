import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// `x instanceof C` calls C's static [Symbol.hasInstance] with x: class code
// receiving the left operand. An object holding the service is tested, so the
// service reaches class code and Store keeps every method. Plain data tested
// against C, and an error tested against Error, hand nothing over: Other is
// read only directly and still prunes.
const Store = Context.Service('test/ClassHoleHasInstanceStore');
const Other = Context.Service('test/ClassHoleHasInstanceOther');
class Checked {
  static [Symbol.hasInstance](value) {
    return value.service?.hasInstanceUsed() === 'used';
  }
}

const program = Effect.gen(function* () {
  const store = yield* Store;
  const other = yield* Other;
  return [
    { service: store } instanceof Checked,
    { label: 'plain' } instanceof Checked,
    new Error('e') instanceof Error,
    other.hasInstanceOtherUsed(),
  ];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    Store,
    Store.of({ hasInstanceUsed: () => 'used', hasInstanceUnread: () => 'unread' }),
  ),
  Layer.succeed(
    Other,
    Other.of({ hasInstanceOtherUsed: () => 'other', hasInstanceOtherUnused: () => 'unused' }),
  ),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  true,
  false,
  true,
  'other',
]);
