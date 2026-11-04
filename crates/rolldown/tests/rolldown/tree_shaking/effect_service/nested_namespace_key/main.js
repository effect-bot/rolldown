import assert from "node:assert"
import * as Effect from "effect/Effect"
import * as B from "./barrel.js"

// The key is retargeted through a two-hop namespace path, so yielding it finds
// the service provided for "test/NestedOther".
function retarget(key) {
  key.key = "test/NestedOther"
}
retarget(B.Provider.K)

const program = Effect.gen(function* () {
  return (yield* B.Provider.K).read()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, B.Provider.layer)), 1)
