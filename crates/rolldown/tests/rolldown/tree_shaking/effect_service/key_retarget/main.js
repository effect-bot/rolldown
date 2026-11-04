import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Keys retargeted by opaque code: after this, yielding Helper or Computed
// finds the services provided for the "other" keys.
const Helper = Context.Service("test/Helper")
const Computed = Context.Service("test/Computed")
const HelperTarget = Context.Service("test/HelperTarget")
const ComputedTarget = Context.Service("test/ComputedTarget")

function retarget(key, to) {
  key.key = to
}
retarget(Helper, "test/HelperTarget")
Computed[["key"].join("")] = "test/ComputedTarget"

const layer = Layer.mergeAll(
  Layer.succeed(HelperTarget, { read: () => 1, unused: () => 2 }),
  Layer.succeed(ComputedTarget, { read: () => 3, unused: () => 4 })
)

const program = Effect.gen(function* () {
  return [(yield* Helper).read(), (yield* Computed).read()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [1, 3])
