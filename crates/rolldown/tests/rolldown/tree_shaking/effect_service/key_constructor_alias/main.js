import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Keys created through an alias of `Context.Service` share the key string of
// the provided service, so yielding them finds it.
const factory = Context.Service
const Hidden = factory("test/Aliased")
const K = Context.Service("test/Aliased")

const layer = Layer.effect(
  K,
  Effect.gen(function* () {
    return { read: () => 1, unused: () => 2 }
  })
)

const program = Effect.gen(function* () {
  return (yield* Hidden).read()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 1)
