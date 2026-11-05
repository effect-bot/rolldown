import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A code loader in a module of the graph that never runs: nothing imports the
// function, so Rolldown removes it. It cannot reach a service, and the unread
// method can go.
import { double } from "./helpers.js"

const Store = Context.Service("test/RealDeadLoaderUnreachable")
const deadUnusedHelper = () => "deadUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const deadUnused = () => deadUnusedHelper()
    return Store.of({ readOne, deadUnused })
  })
)


const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), double(2)]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", 4])
