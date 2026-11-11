import * as Context from 'effect/Context';
import { register } from './ext/reg.js';

const Store = Context.Service('test/EscapeExternalThenHost');
const ctx = Context.make(Store, { used: () => 'used', unused: () => 'unused' });
const lookup = (name) => Context.get(ctx, Context.Service(name)).used();
register(lookup);
// Read through a path so the host call is analysed after the external one.
const a = { x: { y: { z: lookup } } };
process.zzHost(a.x.y.z);
