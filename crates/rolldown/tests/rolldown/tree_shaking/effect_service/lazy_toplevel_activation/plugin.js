import { addHook } from './hooks.js';

// A declaration whose initialiser has a side effect. Nothing imports
// `installed`, but the statement still runs and registers the hook.
export const installed = addHook((store) => store.initializerUsed());
