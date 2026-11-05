import * as Context from 'effect/Context';

const Store = Context.Service('test/RealReexportStore');
const Other = Context.Service('test/RealReexportOther');

// Re-exported under another name and as itself.
export { Store as BillingStore, Other };
