import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// The callee is chosen at run time, but only from the two functions of a
// literal that nothing else writes. Their reads, `readFirst` and `readSecond`,
// are the whole demand; `selectedUnused` can go.
const Store = Context.Service("test/RealSelectedTarget")
const selectedUnusedHelper = () => "c"
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const readFirst = () => "a"
    const readSecond = () => "b"
    const selectedUnused = () => selectedUnusedHelper()
    return Store.of({ readFirst, readSecond, selectedUnused })
  })
)

const handlers = {
  first: (store) => store.readFirst(),
  second: (store) => store.readSecond()
}
const names = "first second".split(" ")

const program = Effect.gen(function* () {
  const store = yield* Store
  return names.map((name) => handlers[name](store))
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), ["a", "b"])
