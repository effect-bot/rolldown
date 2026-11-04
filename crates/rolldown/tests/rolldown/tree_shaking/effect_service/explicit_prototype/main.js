import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The provider sets its prototype explicitly. `read` is inherited from it and
// reads `this.unused` from the service object.
const K = Context.Service("test/ExplicitPrototype")

const proto = () => 0
proto.read = function () {
  return this.unused()
}

const layer = Layer.effect(
  K,
  Effect.gen(function* () {
    return { __proto__: proto, unused: () => 2 }
  })
)

const program = Effect.gen(function* () {
  return (yield* K).read()
})

assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), 2)
