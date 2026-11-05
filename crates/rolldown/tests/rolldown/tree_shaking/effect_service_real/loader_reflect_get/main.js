import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The Function constructor, read through `Reflect.get` with a literal name,
// loads code outside the bundle (A2). The name is a call argument rather than
// a member name, but it is still known at compile time.
const Store = Context.Service("test/RealLoaderReflectGet")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const load = Reflect.get(() => 0, "constructor")

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), load("return 1")()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", 1])
