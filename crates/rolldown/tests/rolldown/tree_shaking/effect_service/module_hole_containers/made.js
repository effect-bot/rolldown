// Imports nothing from `effect`: a module hole until a service, a key or a
// provider reaches it.
export const entriesFrom = (name, make) => new Map([[name, make()]]);
