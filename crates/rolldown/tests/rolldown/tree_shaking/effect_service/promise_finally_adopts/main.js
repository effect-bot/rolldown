import assert from 'node:assert/strict';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
const Store = Context.Service('test/PromiseFinallyAdopts');
let hits = 0;
const service = {
  finallyUsed() {
    hits++;
    return 'done';
  },
  finallyUnused: () => 'unused',
  then(resolve) {
    const self = this;
    resolve({
      then(inner) {
        inner(self.finallyUsed());
      },
    });
  },
};
const store = Effect.runSync(Effect.provideService(Store, Store, service));
assert.equal(await Promise.resolve('done').finally(() => store), 'done');
assert.equal(hits, 1);
