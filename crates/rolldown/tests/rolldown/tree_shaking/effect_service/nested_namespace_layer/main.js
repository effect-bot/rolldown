import assert from "node:assert"
import * as B from "./barrel.js"

// The Layer is built by hand through a two-hop namespace path, and the
// service is read from the resulting Context. Every method must be kept.
const context = await B.Provider.layer.build(new Map(), [])
assert.strictEqual(context.get("test/NestedEscape").unused(), 2)
