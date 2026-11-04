import assert from "node:assert"
import * as Effect from "effect/Effect"
import * as P from "./provider.js"

// The consumer and the provided Layer come from another module's namespace.
const program = Effect.gen(function* () {
  return (yield* P.Store).readOne()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, P.layer)), "live")
