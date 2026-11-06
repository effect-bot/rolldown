// `viaTable` references `table`, whose method references `readTransitive`:
// none of the three statements has a side effect, and reaching the first must
// reach the other two.
const readTransitive = (store) => store.transitiveUsed();
const table = { read: (store) => readTransitive(store) };
export const viaTable = (store) => table.read(store);
