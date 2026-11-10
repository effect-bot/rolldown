// Imports nothing from `effect`: a module hole until a service, a key or a
// provider reaches it.
export const entries = (name, value) => new Map([[name, value]]);
