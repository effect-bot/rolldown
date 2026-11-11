import assert from 'node:assert';
import * as Context from 'effect/Context';

// g1: a service stored on globalThis.
const Store = Context.Service('test/GlobalServiceStore');
const service = { globalUsed: () => 'used', globalUnused: () => 'unused' };
const ctx = Context.make(Store, service);
globalThis.globalService = service;

assert.strictEqual(Context.get(ctx, Store).globalUsed(), 'used');
