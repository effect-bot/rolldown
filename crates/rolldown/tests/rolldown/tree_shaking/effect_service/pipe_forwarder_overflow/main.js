import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { pipeArguments } from 'effect/Pipeable';

// A bundled pipe forwarder, written the way Effect's own data types write it,
// called at more call sites than the clone cap allows. The two calls after
// the filler each read one method of their own receiver's service: the other
// method of each service is removed.
const Proto = {
  pipe() {
    return pipeArguments(this, arguments);
  },
};
const read = (x) => x.value;
const filler = Object.create(Proto);
filler.value = 0;
const fill = [
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
  filler.pipe(read),
];

const A = Context.Service('test/PipeOverflowA');
const B = Context.Service('test/PipeOverflowB');
const aBarHelper = () => 'unused a';
const bFooHelper = () => 'unused b';
const layer = Layer.mergeAll(
  Layer.succeed(A, { foo: () => 'a', bar: () => aBarHelper() }),
  Layer.succeed(B, { foo: () => bFooHelper(), bar: () => 'b' }),
);

const program = Effect.gen(function* () {
  const a = Object.create(Proto);
  a.value = yield* A;
  const b = Object.create(Proto);
  b.value = yield* B;
  return [a.pipe((x) => x.value.foo()), b.pipe((x) => x.value.bar()), fill.length];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), ['a', 'b', 66]);
