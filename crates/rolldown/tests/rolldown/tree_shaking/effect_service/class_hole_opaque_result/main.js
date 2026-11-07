import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A class built from plain data makes an object, and the program hands it a
// service through that object's method. What a class makes is class code's
// value, so Registered keeps every method. Direct is only read directly and
// still prunes. The bundle runs.
const Registered = Context.Service('test/ClassHoleOpaqueResultRegistered');
const Direct = Context.Service('test/ClassHoleOpaqueResultDirect');
class Factory {
  constructor(options) {
    this.options = options;
  }
  make() {
    const label = this.options.label;
    return {
      register(service) {
        this.service = service;
        return label;
      },
    };
  }
}

const program = Effect.gen(function* () {
  const result = new Factory({ label: 'plain' }).make();
  const label = result.register(yield* Registered);
  const direct = yield* Direct;
  return [label, direct.directUsed()];
});

const layer = Layer.mergeAll(
  Layer.succeed(
    Registered,
    Registered.of({ registeredFirst: () => 'f', registeredSecond: () => 's' }),
  ),
  Layer.succeed(Direct, Direct.of({ directUsed: () => 'd', directUnused: () => 'u' })),
);
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['plain', 'd']);
