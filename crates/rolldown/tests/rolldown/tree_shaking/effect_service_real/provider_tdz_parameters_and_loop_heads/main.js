import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// A provider property reads a binding that is still in its temporal dead zone:
// a parameter inside its own default, a later parameter from an earlier
// default, and a lexical loop-head binding while the loop's right-hand side
// runs. Building each provider throws. Removing the unread property would
// drop that exception.
const SelfDefault = Context.Service("test/RealTdzParameterSelfDefault")
const LaterParameter = Context.Service("test/RealTdzParameterLater")
const ForOfHead = Context.Service("test/RealTdzForOfHead")
const ForInHead = Context.Service("test/RealTdzForInHead")

const readSelfDefault = Effect.gen(function* () {
  return (yield* SelfDefault).readOne()
})
const readLaterParameter = Effect.gen(function* () {
  return (yield* LaterParameter).readOne()
})
const readForOfHead = Effect.gen(function* () {
  return (yield* ForOfHead).readOne()
})
const readForInHead = Effect.gen(function* () {
  return (yield* ForInHead).readOne()
})

const throwsReferenceError = (run) => {
  try {
    run()
  } catch (error) {
    if (!(error instanceof ReferenceError)) throw error
    return true
  }
  return false
}

assert.strictEqual(
  throwsReferenceError(() => {
    function build(
      selfHelper = Effect.runSync(
        Effect.provide(readSelfDefault, Layer.succeed(SelfDefault, { readOne: () => "read", unused: selfHelper }))
      )
    ) {}
    build()
  }),
  true
)

assert.strictEqual(
  throwsReferenceError(() => {
    const build = (
      first = Effect.runSync(
        Effect.provide(
          readLaterParameter,
          Layer.succeed(LaterParameter, { readOne: () => "read", unused: laterHelper })
        )
      ),
      laterHelper = () => "unused"
    ) => first
    build()
  }),
  true
)

assert.strictEqual(
  throwsReferenceError(() => {
    for (const ofHelper of [
      Effect.runSync(Effect.provide(readForOfHead, Layer.succeed(ForOfHead, { readOne: () => "read", unused: ofHelper })))
    ]) {
    }
  }),
  true
)

assert.strictEqual(
  throwsReferenceError(() => {
    for (let inHelper in {
      key: Effect.runSync(Effect.provide(readForInHead, Layer.succeed(ForInHead, { readOne: () => "read", unused: inHelper })))
    }) {
    }
  }),
  true
)
