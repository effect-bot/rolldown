import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// `Effect.withFiber` hands its closure the running fiber. The closure
// enumerates the fiber's Context and calls `ping` on every service that has
// it. `ping` is demanded of every service; other unread methods can go.
const Store = Context.Service("test/RealWithFiberEnumeration")
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const ping = () => "pong"
    const fiberUnused = () => "unused"
    return Store.of({ readOne, ping, fiberUnused })
  })
)

const pings = Effect.withFiber((fiber) => {
  const out = []
  for (const service of fiber.context.mapUnsafe.values()) {
    if (typeof service.ping === "function") out.push(service.ping())
  }
  return Effect.succeed(out)
})

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), yield* pings]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", ["pong"]])
