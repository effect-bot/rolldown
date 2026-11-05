import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

const Store = Context.Service("test/RealSharedallocStore")
const sharedAllocUnusedHelper = () => "u"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "r"
    const sharedAllocUnused = () => sharedAllocUnusedHelper()
    return Store.of({ readOne, sharedAllocUnused })
  })
)
const name = ["sharedAlloc", "Unused"].join("")
// The provided value is plain data, but the program stores the Context into
// the same allocation later: enumerating it through the service reads every
// service.
const shared = { context: undefined }
const Box = Context.Service("test/RealPerKeySharedAllocationBox")
const BoxLayer = Layer.succeed(Box, Box.of({ shared }))
const program = Effect.gen(function* () {
  shared.context = yield* Effect.context()
  const box = yield* Box
  const out = []
  for (const service of box.shared.context.mapUnsafe.values()) {
    if (typeof service[name] === "function") out.push(service[name]())
  }
  return [(yield* Store).readOne(), out]
})
assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.merge(BoxLayer, StoreLayer))),
  ["r", ["u"]]
)
