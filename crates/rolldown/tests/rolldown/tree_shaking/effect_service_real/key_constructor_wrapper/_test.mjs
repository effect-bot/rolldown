import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Both keys are tracked: the unread methods are removed.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of []) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["wrapperUnused", "wrapperUnusedHelper", "aliasUnused", "aliasUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
