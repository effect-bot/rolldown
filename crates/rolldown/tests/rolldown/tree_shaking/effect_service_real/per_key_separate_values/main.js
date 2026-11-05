import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

const Store = Context.Service("test/RealSeparateStore")
const separateUnusedHelper = () => "u"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "r"
    const separateUnused = () => separateUnusedHelper()
    return Store.of({ readOne, separateUnused })
  })
)
const name = ["separate", "Unused"].join("")
// Two keys: one service holds the Context, which the program only compares,
// the other holds data the program enumerates. No runtime object is
// enumerated or converted, so every unread method is removed.
const Box = Context.Service("test/RealPerKeySeparateBox")
const BoxLayer = Layer.effect(
  Box,
  Effect.map(Effect.context(), (context) => Box.of({ context }))
)
const Data = Context.Service("test/RealPerKeySeparateData")
const DataLayer = Layer.succeed(Data, Data.of({ rows: { a: 1, b: 2 } }))
const AppLayer = Layer.mergeAll(Layer.provideMerge(BoxLayer, StoreLayer), DataLayer)
const program = Effect.gen(function* () {
  const box = yield* Box
  const data = yield* Data
  const keys = []
  for (const key in data.rows) keys.push(`${key}${data.rows[key]}`)
  return [box.context !== undefined, (yield* Store).readOne(), keys]
})
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, AppLayer)), [true, "r", ["a1", "b2"]])
