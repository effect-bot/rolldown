import * as Context from "effect/Context"

const Store = Context.Service("test/RealReexportOpaqueStore")

export { Store as BillingStore }
