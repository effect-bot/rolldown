import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

const Store = Context.Service("test/RealMergeallStore")
const mergeAllUnusedHelper = () => "u"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "r"
    const mergeAllUnused = () => mergeAllUnusedHelper()
    return Store.of({ readOne, mergeAllUnused })
  })
)
const name = ["mergeAll", "Unused"].join("")
// `Layer.mergeAll` over spread arguments builds the service holding the
// Context it is built in, which has the store.
const Box = Context.Service("test/RealMergeAllBox")
const BoxLayer = Layer.effect(
  Box,
  Effect.map(Effect.context(), (context) => Box.of({ context }))
)
const layers = [Layer.provide(BoxLayer, StoreLayer), StoreLayer]
const AppLayer = Layer.mergeAll(...layers)
const program = Effect.gen(function* () {
  const box = yield* Box
  const out = []
  for (const service of box.context.mapUnsafe.values()) {
    if (typeof service[name] === "function") out.push(service[name]())
  }
  return [(yield* Store).readOne(), out]
})
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), ["r", ["u"]])
