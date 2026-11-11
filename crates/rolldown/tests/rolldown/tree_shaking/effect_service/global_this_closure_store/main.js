import assert from 'node:assert';
import * as Context from 'effect/Context';

// g1: a function stored on globalThis returns a service.
const Store = Context.Service('test/GlobalClosureStore');
const ctx = Context.make(Store, { closureUsed: () => 'used', closureUnused: () => 'unused' });
globalThis.globalLookup = () => Context.get(ctx, Store);

assert.strictEqual(globalThis.globalLookup().closureUsed(), 'used');
