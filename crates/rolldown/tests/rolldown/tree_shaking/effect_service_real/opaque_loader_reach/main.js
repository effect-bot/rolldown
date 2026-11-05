import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Run A compiles code from text built at run time and passes it one service.
// While a fiber runs, Effect keeps it in
// `globalThis["~effect/Fiber/currentFiber"]`, so the compiled code also reaches
// the other service of run A without being handed it. Both services of run A
// keep every method. Run B never runs the compiled code and shares nothing
// with run A, so its unread method can go.
const Store = Context.Service("test/RealOpaqueLoaderStore")
const Ambient = Context.Service("test/RealOpaqueLoaderAmbient")
const Other = Context.Service("test/RealOpaqueLoaderOther")
const loaderStoreUnusedHelper = () => "s2"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readStore = () => "s1"
    const loaderStoreUnused = () => loaderStoreUnusedHelper()
    return Store.of({ readStore, loaderStoreUnused })
  })
)
const loaderAmbientUnusedHelper = () => "m2"
const AmbientLayer = Layer.effect(
  Ambient,
  Effect.gen(function* () {
    const readAmbient = () => "m1"
    const loaderAmbientUnused = () => loaderAmbientUnusedHelper()
    return Ambient.of({ readAmbient, loaderAmbientUnused })
  })
)
const loaderOtherUnusedHelper = () => "o2"
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => "o1"
    const loaderOtherUnused = () => loaderOtherUnusedHelper()
    return Other.of({ readOther, loaderOtherUnused })
  })
)

const source = [
  "const fiber = globalThis['~effect/Fiber/current', 'Fiber'].context;",
  "return [store.loaderStore', 'Unused(), fiber.mapUnsafe.get('test/RealOpaqueLoader', 'Ambient').loaderAmbient', 'Unused()]"
]
  .join("")
  .split("', '")
  .join("")
const runA = Effect.gen(function* () {
  const store = yield* Store
  const ambient = yield* Ambient
  return [store.readStore(), ambient.readAmbient(), ...Function("store", source)(store)]
})
const runB = Effect.gen(function* () {
  return (yield* Other).readOther()
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(runA, Layer.mergeAll(StoreLayer, AmbientLayer))),
  ["s1", "m1", "s2", "m2"]
)
assert.strictEqual(await Effect.runPromise(Effect.provide(runB, OtherLayer)), "o1")
