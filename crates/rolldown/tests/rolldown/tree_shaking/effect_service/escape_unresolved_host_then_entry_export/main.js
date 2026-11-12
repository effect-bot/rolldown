import assert from 'node:assert/strict';
import * as Context from 'effect/Context';

const Store = Context.Service('test/ZzFl');
export const svc = { flUsed: () => 'used', flUnused: () => 'unused' };
const ctx = Context.make(Store, svc);
zzHost(svc); // an unresolved global: a host escape when recorded, code the pass can't see since the escape batch (commit 3), so no longer the host-first path
assert.strictEqual(Context.get(ctx, Store).flUsed(), 'used');
