const constructorFnUnusedHelper = () => 'unused';

// Builds the client object: the provider's object literal is returned from here.
export const build = (http) => ({
  http,
  get: (path) => http.request(path),
  constructorFnUnused: () => constructorFnUnusedHelper(),
});
