import * as Context from 'effect/Context';
import { register } from './ext/reg.js';

const Store = Context.Service('test/ZzEh');
const ctx = Context.make(Store, { ehUsed: () => 'used', ehUnused: () => 'unused' });
const lookup = (name) => Context.get(ctx, Context.Service(name)).ehUsed();
register(lookup);
const a = { x: { y: { z: lookup } } };
zzHost(a.x.y.z);
