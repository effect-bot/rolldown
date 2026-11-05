import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Run B's service keeps every method: a getter installed by run A's compiled code reads it.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["accessorOtherUnused", "accessorOtherUnusedHelper"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of []) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
