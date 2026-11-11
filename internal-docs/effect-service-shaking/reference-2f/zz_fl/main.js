import assert from 'node:assert/strict';
import * as Context from 'effect/Context';

const Store = Context.Service('test/ZzFl');
export const svc = { flUsed: () => 'used', flUnused: () => 'unused' };
const ctx = Context.make(Store, svc);
zzHost(svc); // an unresolved global, so a Top(Host) call
assert.strictEqual(Context.get(ctx, Store).flUsed(), 'used');
