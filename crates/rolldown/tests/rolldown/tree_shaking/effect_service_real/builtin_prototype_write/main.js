import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Replacing Map.prototype.get captures the service map of a Context when Effect looks a
// service up. Every method of the service must be kept.
const Store = Context.Service("test/RealPrototypeWrite")

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => "unused"
    return Store.of({ readOne, unused })
  })
)

const maps = []
const originalGet = Map.prototype.get
Map.prototype.get = function (key) {
  maps.push(this)
  return originalGet.call(this, key)
}

const observe = () => {
  Map.prototype.get = originalGet
  const map = maps.find((map) => map.has("test/RealPrototypeWrite"))
  return originalGet.call(map, "test/RealPrototypeWrite").unused()
}

const program = Effect.gen(function* () {
  const store = yield* Store
  // The live Context of the provided scope, through the running Fiber.
  const context = yield* Effect.withFiber((fiber) => Effect.succeed(fiber.context))
  return [store.readOne(), observe(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
