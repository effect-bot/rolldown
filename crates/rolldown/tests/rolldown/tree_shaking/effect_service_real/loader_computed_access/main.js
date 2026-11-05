import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A code loader reached through non-constant names on the host `process` still
// loads code outside the bundle (A2).
const Store = Context.Service("test/RealLoaderComputedAccess")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const load = () =>
  process[["getBuiltin", "Module"].join("")]("vm")[["compile", "Function"].join("")]("return 1")()

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), load()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", 1])
