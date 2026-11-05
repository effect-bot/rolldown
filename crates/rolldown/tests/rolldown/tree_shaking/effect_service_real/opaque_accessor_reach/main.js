import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: run A hands a plain object to code compiled from text built at run
// time, and that code turns one of its fields into a getter. Run B reads the
// field statically, so the getter runs inside run B's fiber and reaches run
// B's service through `globalThis["~effect/Fiber/currentFiber"]`. Run B makes
// no call into the compiled code, yet its service keeps every method.
const Store = Context.Service("test/RealOpaqueAccessorStore")
const Other = Context.Service("test/RealOpaqueAccessorOther")
const StoreLayer = Layer.succeed(Store, Store.of({ readStore: () => "s1" }))
const accessorOtherUnusedHelper = () => "o2"
const OtherLayer = Layer.effect(
  Other,
  Effect.gen(function* () {
    const readOther = () => "o1"
    const accessorOtherUnused = () => accessorOtherUnusedHelper()
    return Other.of({ readOther, accessorOtherUnused })
  })
)

const config = { label: "plain" }
const source = [
  "Object.defineProperty(config, 'label', { get() {",
  " const context = globalThis['~effect/Fiber/current', 'Fiber'].context;",
  " return context.mapUnsafe.get('test/RealOpaqueAccessor', 'Other').accessorOther', 'Unused() } })"
]
  .join("")
  .split("', '")
  .join("")
const runA = Effect.gen(function* () {
  Function("config", source)(config)
  return (yield* Store).readStore()
})
const runB = Effect.gen(function* () {
  return [(yield* Other).readOther(), config.label]
})

assert.strictEqual(await Effect.runPromise(Effect.provide(runA, StoreLayer)), "s1")
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(runB, OtherLayer)), ["o1", "o2"])
