import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The Function constructor, reached through a constant computed name on an
// ordinary function, loads code outside the bundle (A2). A name known only at
// run time, such as `["con", "structor"].join("")`, is outside the contract:
// the app is assumed not to build one, and the pass does not detect it.
const Store = Context.Service("test/RealLoaderFunctionConstructorArrow")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const load = (() => 0)["con" + "structor"]

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), load("return 1")()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", 1])
