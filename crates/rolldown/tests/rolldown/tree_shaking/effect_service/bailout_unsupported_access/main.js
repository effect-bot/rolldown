import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Each service is provided in the base-case shape but consumed through a path
// outside the base case, so every one of them keeps `unused`.
const A = Context.Service("test/A") // combinator: Effect.map(A, f)
const B = Context.Service("test/B") // direct Context access
const C = Context.Service("test/C") // Key.use
const D = Context.Service("test/D") // the service escapes to a function
const E = Context.Service("test/E") // computed member access
const F = Context.Service("test/F") // a method reads `this`

const unusedA = () => "unusedA"
const unusedB = () => "unusedB"
const unusedC = () => "unusedC"
const unusedD = () => "unusedD"
const unusedE = () => "unusedE"
const unusedF = () => "unusedF"

const provider = (key, unused) =>
  Layer.effect(
    key,
    Effect.gen(function* () {
      const used = () => "used"
      return key.of({ used, unused })
    })
  )

const FLayer = Layer.effect(
  F,
  Effect.gen(function* () {
    return F.of({
      used() {
        return this.unused()
      },
      unused: () => unusedF()
    })
  })
)

const layer = Layer.mergeAll(
  provider(A, () => unusedA()),
  provider(B, () => unusedB()),
  provider(C, () => unusedC()),
  provider(D, () => unusedD()),
  provider(E, () => unusedE()),
  FLayer
)

const describe = (service) => Object.keys(service).join(",")

const program = Effect.gen(function* () {
  const a = yield* Effect.map(A, (a) => a.used())
  const b = yield* Effect.flatMap(Effect.context(), (context) => Effect.succeed(Context.get(context, B).used()))
  const c = yield* C.use((c) => Effect.succeed(c.used()))
  const d = describe(yield* D)
  const e = yield* E
  const name = ["un", "used"].join("")
  const f = yield* F
  return [a, b, c, d, e[name](), f.used()]
})

assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, layer)), [
  "used",
  "used",
  "used",
  "used,unused",
  "unusedE",
  "unusedF"
])
