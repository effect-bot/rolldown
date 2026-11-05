const unusedHelper = () => "unused"

// Builds the client object for the provider and for a direct caller.
export const build = () => ({
  get: () => "got",
  unused: () => unusedHelper()
})
