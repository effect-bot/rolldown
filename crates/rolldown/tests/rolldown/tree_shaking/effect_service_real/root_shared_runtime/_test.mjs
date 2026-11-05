import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// The service shared by both runs keeps every method; the service of run A alone loses its unread one.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["sharedRuntimeUnused", "sharedRuntimeUnusedHelper"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["localRuntimeUnused", "localRuntimeUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
