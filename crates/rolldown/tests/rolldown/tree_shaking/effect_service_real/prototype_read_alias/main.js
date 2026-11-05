import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Builtin prototypes used as values without being written: compared against
// and used as the prototype of a new object, as Distilled core does. The
// unread method can go.
const Store = Context.Service("test/RealPrototypeRead")
const isPlainObject = (value) => Object.getPrototypeOf(value) === Object.prototype
const callableStatics = Object.create(Function.prototype)
const protoUnusedHelper = () => "protoUnused"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readOne = () => "readOne"
    const protoUnused = () => protoUnusedHelper()
    return Store.of({ readOne, protoUnused })
  })
)

const program = Effect.gen(function* () {
  return [(yield* Store).readOne(), isPlainObject({}), typeof callableStatics.call]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["readOne", true, "function"])
