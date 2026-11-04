import assert from "node:assert"
import { layer } from "./provider.js"

// Control for namespace_layer_escape: the same escape through a named import.
const context = await layer.build(new Map(), [])
const service = context.get("test/Escape")
assert.strictEqual(service.unused(), 2)
