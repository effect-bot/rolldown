import assert from 'node:assert';
import * as Context from 'effect/Context';

// The trap is assigned to the handler after the Proxy rule handed the handler to the host.
const Store = Context.Service('test/ProxyHandlerAssigned');
const ctx = Context.make(Store, { proxyUsed: () => 'used', proxyUnused: () => 'unused' });
const handler = {};
const services = new Proxy({}, handler);
handler.get = (_target, name) => Context.get(ctx, Context.Service(name)).proxyUsed();

assert.strictEqual(services['test/ProxyHandlerAssigned'], 'used');
