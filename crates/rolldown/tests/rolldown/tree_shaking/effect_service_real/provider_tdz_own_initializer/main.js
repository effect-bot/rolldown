import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A provider property reads a binding inside that binding's own initializer,
// directly and from an immediately invoked function. The binding is still in
// its temporal dead zone, so building the provider throws. Removing the unread
// property would drop that exception.
const Direct = Context.Service("test/RealProviderTdzOwnInitializerDirect")
const Nested = Context.Service("test/RealProviderTdzOwnInitializerNested")

const readDirect = Effect.gen(function* () {
  return (yield* Direct).readOne()
})

let directCaught = false
try {
  const directHelper = Effect.runSync(
    Effect.provide(readDirect, Layer.succeed(Direct, { readOne: () => "read", unused: directHelper }))
  )
} catch (error) {
  if (!(error instanceof ReferenceError)) throw error
  directCaught = true
}
assert.strictEqual(directCaught, true)

let nestedCaught = false
try {
  const nestedHelper = (() => {
    const layer = Layer.succeed(Nested, { readOne: () => "read", unused: nestedHelper })
    const readNested = Effect.gen(function* () {
      return (yield* Nested).readOne()
    })
    Effect.runSync(Effect.provide(readNested, layer))
    return () => "unused"
  })()
} catch (error) {
  if (!(error instanceof ReferenceError)) throw error
  nestedCaught = true
}
assert.strictEqual(nestedCaught, true)
