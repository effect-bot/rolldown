import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"
import { compile } from "./loader.js"

// A code loader reached through a renamed re-export of `node:vm` still loads code
// outside the bundle (A2).
const Store = Context.Service("test/RealLoaderReexport")

const unusedHelper = () => "unused"

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "read"
    const unused = () => unusedHelper()
    return Store.of({ readOne, unused })
  })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  return [store.readOne(), compile("return 1")()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["read", 1])
