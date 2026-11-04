import * as Context from "effect/Context"
import * as Layer from "effect/Layer"

// The key stays private to this module; only the Layer is exported.
const K = Context.Service("test/NestedEscape")

export const layer = Layer.succeed(K, { read: () => 1, unused: () => 2 })
