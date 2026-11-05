import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// While a fiber runs, Effect keeps it in
// `globalThis["~effect/Fiber/currentFiber"]` (internal/effect.js, runLoop).
// Code inside the run reads that global by a constant name, enumerates the
// fiber's Context and calls `ping` on every service that has it. `ping` is
// demanded of every service in the run; other unread methods can go.
const Store = Context.Service("test/RealAmbientFiber")
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const ping = () => "pong"
    const ambientUnused = () => "unused"
    return Store.of({ readOne, ping, ambientUnused })
  })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  const context = globalThis["~effect/Fiber/currentFiber"].context
  const pings = []
  for (const service of context.mapUnsafe.values()) {
    if (typeof service.ping === "function") pings.push(service.ping())
  }
  return [store.readOne(), pings]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", ["pong"]])
