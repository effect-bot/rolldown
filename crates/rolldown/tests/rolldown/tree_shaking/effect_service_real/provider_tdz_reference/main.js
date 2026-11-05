import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A provider property reads a binding still in its temporal dead zone, so building
// the provider throws. Removing the unread property would drop that exception.
const Store = Context.Service("test/RealProviderTdz")

const program = Effect.gen(function* () {
  return (yield* Store).readOne()
})

let caught = false
try {
  const layer = Layer.succeed(Store, {
    readOne: () => "read",
    unused: helper
  })
  const helper = () => "unused"
  await Effect.runPromise(Effect.provide(program, layer))
} catch (error) {
  caught = error instanceof ReferenceError
}
assert.strictEqual(caught, true)
