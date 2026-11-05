import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Both candidate handlers' reads are kept; the method neither reads is removed.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["readFirst", "readSecond"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["selectedUnused", "selectedUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
