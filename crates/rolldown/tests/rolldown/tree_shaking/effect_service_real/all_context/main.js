import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Effect.all collects the Context with another success.
const Store = Context.Service("test/RealAllContext")
const allUnusedHelper = () => "u"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "r"
    const allUnused = () => allUnusedHelper()
    return Store.of({ readOne, allUnused })
  })
)

// Calls, on every service of a Context, the method named at run time.
const name = ["all", "Unused"].join("")
const callAll = (context) => {
  const out = []
  for (const service of context.mapUnsafe.values()) {
    if (typeof service[name] === "function") out.push(service[name]())
  }
  return out
}

const program = Effect.gen(function* () {
  const [context, one] = yield* Effect.all([Effect.context(), Effect.succeed(1)])
  return [(yield* Store).readOne(), one, callAll(context)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["r", 1, ["u"]])
