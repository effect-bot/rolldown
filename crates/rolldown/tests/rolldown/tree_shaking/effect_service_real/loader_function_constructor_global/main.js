import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The Function constructor, reached through a non-constant name on a global
// builtin, loads code outside the bundle (A2).
const Store = Context.Service("test/RealLoaderFunctionConstructorGlobal")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const load = globalThis.Object[["con", "structor"].join("")]

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), load("return 1")()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", 1])
