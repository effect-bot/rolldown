import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Positive gate: the pass must actually remove code here, so a global bailout
// cannot make the suite pass.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["deadHelper", "writeOne"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
for (const name of ["liveHelper", "readOne"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
