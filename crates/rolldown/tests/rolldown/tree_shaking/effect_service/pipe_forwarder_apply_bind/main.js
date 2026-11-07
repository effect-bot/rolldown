import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import { pipeArguments } from 'effect/Pipeable';

// A bundled pipe forwarder called with another receiver through `.apply`,
// `.bind` with the function given later, and `.bind` with the function bound
// up front. Each reads one method of the service, which stays; the method
// nothing reads is removed. (`.call` is in pipe_forwarder_guards.)
const Proto = {
  pipe() {
    return pipeArguments(this, arguments);
  },
};

const Store = Context.Service('test/PipeForwarderApplyBind');
const applyBindHelper = () => 'unused';
const StoreLayer = Layer.succeed(Store, {
  viaApply: () => 'a',
  viaBind: () => 'b',
  viaBoundArgs: () => 'ba',
  applyBindUnused: () => applyBindHelper(),
});

const program = Effect.gen(function* () {
  const store = yield* Store;
  return [
    Proto.pipe.apply({ value: store }, [(x) => x.value.viaApply()]),
    Proto.pipe.bind({ value: store })((x) => x.value.viaBind()),
    Proto.pipe.bind({ value: store }, (x) => x.value.viaBoundArgs())(),
  ];
});

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  'a',
  'b',
  'ba',
]);
