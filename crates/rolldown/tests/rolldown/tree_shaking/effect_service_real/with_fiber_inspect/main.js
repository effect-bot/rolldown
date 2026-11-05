import assert from "node:assert"
import { inspect } from "node:util"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: a `withFiber` closure formats the fiber's Context with
// `util.inspect`, which prints every method of every service. The output must
// not change, so every method stays.
const Store = Context.Service("test/RealWithFiberInspect")
const inspectUnusedHelper = () => "unused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const inspectUnused = () => inspectUnusedHelper()
    return Store.of({ readOne, inspectUnused })
  })
)

const describe = Effect.withFiber((fiber) => Effect.succeed(inspect(fiber.context, { depth: 4 })))

const program = Effect.gen(function* () {
  const store = yield* Store
  const text = yield* describe
  return [store.readOne(), text.includes(["inspect", "Unused"].join(""))]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", true])
