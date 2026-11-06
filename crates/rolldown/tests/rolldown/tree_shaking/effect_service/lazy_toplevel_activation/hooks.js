const hooks = [];
export const addHook = (hook) => hooks.push(hook);
export const runHooks = (store) => hooks.map((hook) => hook(store));
