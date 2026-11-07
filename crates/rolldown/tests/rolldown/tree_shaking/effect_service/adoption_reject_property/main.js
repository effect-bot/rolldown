import assert from 'node:assert/strict';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Inspectable from 'effect/Inspectable';
const Store = Context.Service('test/AdoptionRejectProperty');
let hits = 0;
const service = {
  rejectUsed() {
    hits++;
    return 'done';
  },
  rejectUnused: () => 'unused',
  then(resolve) {
    const self = this;
    resolve({
      then(inner, reject) {
        reject.invoke = () => self.rejectUsed();
        inner(reject.invoke());
      },
    });
  },
};
const load = Effect.runSync(
  Effect.provideService(
    Effect.map(Store, (store) => async () => Inspectable.toJson(store)),
    Store,
    service,
  ),
);
assert.equal(await load(), 'done');
assert.equal(hits, 1);
