// Imports nothing from `effect`: a module hole until a service, a key or a
// provider reaches it.
export const passThrough = (value) => value;
