import * as Context from 'effect/Context';

const Store = Context.Service('test/EscapeExportThenHost');
const ctx = Context.make(Store, { used: () => 'used', unused: () => 'unused' });
const lookup = (name) => Context.get(ctx, Context.Service(name)).used();
export { lookup };
// Read through a path so the host call is analysed after the export.
const a = { x: { y: { z: lookup } } };
process.zzHost(a.x.y.z);
