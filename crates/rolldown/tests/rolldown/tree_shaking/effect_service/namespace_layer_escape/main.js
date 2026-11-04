import assert from "node:assert"
import * as P from "./provider.js"

// The Layer is built by hand through a namespace import, and the service is
// read from the resulting Context. Every method must be kept.
const context = await P.layer.build(new Map(), [])
const service = context.get("test/Escape")
assert.strictEqual(service.unused(), 2)
