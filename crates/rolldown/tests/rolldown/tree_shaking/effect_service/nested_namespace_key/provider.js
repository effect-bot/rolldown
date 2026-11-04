import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

export const K = Context.Service("test/NestedKey")
const Other = Context.Service("test/NestedOther")

export const layer = Layer.effect(
  Other,
  Effect.gen(function* () {
    return { read: () => 1, unused: () => 2 }
  })
)
