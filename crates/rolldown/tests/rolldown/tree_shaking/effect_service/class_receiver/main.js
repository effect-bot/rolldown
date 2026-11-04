import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A class heritage expression runs with the method's receiver, so `read`
// reads `this.unused` even though it is inside a class.
const K = Context.Service("test/ClassReceiver")

const layer = Layer.effect(
  K,
  Effect.gen(function* () {
    return {
      read: function () {
        const C = class extends this.unused() {}
        return new C().value
      },
      unused: () =>
        class {
          value = 2
        }
    }
  })
)

const program = Effect.gen(function* () {
  return (yield* K).read()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 2)
