import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Control: a key built at run time with no constant prefix may alias any key.
// Every method of every service stays.
const PrefixStore = Context.Service("test/RealEmptyPrefix/A")
const OtherStore = Context.Service("test/RealEmptyPrefix/B")
const id = ["test/RealEmptyPrefix", "B"].join("/")
const Dynamic = Context.Service(`${id}`)
const emptyPrefixUnusedHelperA = () => "emptyPrefixUnusedA"
const PrefixLayer = Layer.effect(
  PrefixStore,
  Effect.gen(function* () {
    const readPrefix = () => "readPrefix"
    const emptyPrefixUnusedA = () => emptyPrefixUnusedHelperA()
    return PrefixStore.of({ readPrefix, emptyPrefixUnusedA })
  })
)
const emptyPrefixUnusedHelperB = () => "emptyPrefixUnusedB"
const OtherLayer = Layer.effect(
  OtherStore,
  Effect.gen(function* () {
    const readOther = () => "readOther"
    const emptyPrefixUnusedB = () => emptyPrefixUnusedHelperB()
    return OtherStore.of({ readOther, emptyPrefixUnusedB })
  })
)

const program = Effect.gen(function* () {
  return [(yield* PrefixStore).readPrefix(), (yield* OtherStore).readOther(), (yield* Dynamic).emptyPrefixUnusedB()]
})

assert.deepStrictEqual(
  await Effect.runPromise(Effect.provide(program, Layer.mergeAll(PrefixLayer, OtherLayer))),
  ["readPrefix", "readOther", "emptyPrefixUnusedB"]
)
