import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// `Effect.updateServiceScoped` is a keyed lookup like `updateService`: here by a
// hand-made key, and the callback calls the otherwise-unused method.
const Store = Context.Service("test/RealUpdateServiceScoped")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const program = Effect.scoped(
  Effect.gen(function* () {
    const store = yield* Store
    let observed
    yield* Effect.updateServiceScoped({ key: "test/RealUpdateServiceScoped" }, (old) => {
      observed = old.unused()
      return old
    })
    return [store.readOne(), observed]
  })
)

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
