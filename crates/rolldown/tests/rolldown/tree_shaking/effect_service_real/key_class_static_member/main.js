import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A class-form key whose body reads the service through `this`, a use of the
// key the pass does not model. Every method of the service stays.
class Store extends Context.Service()("test/RealKeyClassStaticMember") {
  static readUnused() {
    return Effect.map(this, (store) => store.unused())
  }
}

const unusedHelper = () => "unused"

const StoreLayer = Layer.succeed(Store, { readOne: () => "read", unused: () => unusedHelper() })

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), yield* Store.readUnused()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", "unused"])
