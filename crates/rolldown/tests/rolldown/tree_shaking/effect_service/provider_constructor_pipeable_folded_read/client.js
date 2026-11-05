const pipeableUnusedHelper = () => 'unused';

// Builds the client object for the provider.
export const build = () => ({
  get: () => 'got',
  pipeableRead: () => 'read by the pipeable',
  pipeableUnused: () => pipeableUnusedHelper(),
});
