import assert from 'node:assert';
import * as Context from 'effect/Context';
import { run as viaImport } from './esm.js';
import { run as viaCommonJs } from './imported.cjs';

const { run: viaRequire } = require('./required.cjs');
const { run: viaDynamic } = await import('./dynamic.js');

// A callback handed to a module that never imports `effect` makes the key from
// the name that module passes, and calls a method through it. Each loader
// reaches its own module, and each method stays; the unread one goes.
const Store = Context.Service('test/ModuleHoleCallbackKey');
const ctx = Context.make(Store, {
  holeGet: () => 'got',
  holeImport: () => 'import',
  holeCommonJs: () => 'commonjs',
  holeRequire: () => 'require',
  holeDynamic: () => 'dynamic',
  holeUnused: () => 'unused',
});
const lookup = (name) => Context.get(ctx, Context.Service(name));

assert.strictEqual(Context.get(ctx, Store).holeGet(), 'got');
assert.strictEqual(
  viaImport((name) => lookup(name).holeImport()),
  'import',
);
assert.strictEqual(
  viaCommonJs((name) => lookup(name).holeCommonJs()),
  'commonjs',
);
assert.strictEqual(
  viaRequire((name) => lookup(name).holeRequire()),
  'require',
);
assert.strictEqual(
  viaDynamic((name) => lookup(name).holeDynamic()),
  'dynamic',
);
