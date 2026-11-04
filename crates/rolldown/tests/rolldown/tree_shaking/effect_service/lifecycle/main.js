import assert from "node:assert"
import * as Context from "effect/Context"
import * as Effect from "effect/Effect"
import * as Layer from "effect/Layer"

// Everything `writeOne` uses is created while constructing: the Audit service
// is acquired, `dependency.prepare()` and `makeCache()` are called, and
// `describe` is also called during construction. All of that must still run
// after `writeOne` is removed; only `writeOne` and code it alone reaches go.
const Store = Context.Service("test/Store")
const Dependency = Context.Service("test/Dependency")
const Audit = Context.Service("test/Audit")

const log = []

const dependencyRead = () => "read"
const dependencyPrepare = () => {
  log.push("prepared")
  return "prepared"
}
const dependencyDescribe = () => "described"
const dependencyWrite = () => "write"
const auditRecord = () => "audited"
const writeHelper = (value) => `wrote ${value}`
const makeCache = () => {
  log.push("cache made")
  return new Map()
}

const DependencyLayer = Layer.succeed(Dependency, {
  read: dependencyRead,
  prepare: dependencyPrepare,
  describe: dependencyDescribe,
  write: dependencyWrite
})
const AuditLayer = Layer.effect(
  Audit,
  Effect.acquireRelease(
    Effect.sync(() => {
      log.push("audit acquired")
      return { record: auditRecord }
    }),
    () => Effect.sync(() => log.push("audit released"))
  )
)

const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const dependency = yield* Dependency
    const audit = yield* Audit
    yield* Effect.sync(() => log.push("constructed"))
    const prepared = dependency.prepare()
    const cache = makeCache()
    const describe = (label) => `${label}:${dependency.describe()}`
    log.push(describe("init"))
    const readOne = () => dependency.read()
    const writeOne = () => {
      cache.set(prepared, dependency.write())
      return writeHelper(`${describe("write")}:${audit.record()}`)
    }
    return Store.of({ readOne, writeOne })
  })
)

const program = Effect.gen(function* () {
  const store = yield* Store
  return store.readOne()
})

const layer = Layer.provide(StoreLayer, Layer.mergeAll(DependencyLayer, AuditLayer))
assert.strictEqual(await Effect.runPromise(Effect.provide(program, layer)), "read")
assert.deepStrictEqual(log, [
  "audit acquired",
  "constructed",
  "prepared",
  "cache made",
  "init:described",
  "audit released"
])
