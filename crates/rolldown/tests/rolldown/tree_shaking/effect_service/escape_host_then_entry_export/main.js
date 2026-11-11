import assert from 'node:assert/strict';
import * as Context from 'effect/Context';

const Store = Context.Service('test/EscapeHostThenExport');
export const svc = { used: () => 'used', unused: () => 'unused' };
const ctx = Context.make(Store, svc);
// `process` is a host builtin by classification: its members take host escapes.
process.zzHost(svc);
assert.strictEqual(Context.get(ctx, Store).used(), 'used');
