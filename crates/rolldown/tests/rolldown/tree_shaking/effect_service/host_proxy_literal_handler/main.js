import assert from 'node:assert';
import * as Context from 'effect/Context';

// The handler literal reaches the Proxy rule before its `get` trap is stored on it.
const Store = Context.Service('test/ProxyLiteralHandler');
const ctx = Context.make(Store, { proxyUsed: () => 'used', proxyUnused: () => 'unused' });
const services = new Proxy(
  {},
  {
    get: (_target, name) => Context.get(ctx, Context.Service(name)).proxyUsed(),
  },
);

assert.strictEqual(services['test/ProxyLiteralHandler'], 'used');
