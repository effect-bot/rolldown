import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Run A's services keep every method; run B's service loses its unread one.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["loaderStoreUnused", "loaderStoreUnusedHelper", "loaderAmbientUnused", "loaderAmbientUnusedHelper"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["loaderOtherUnused", "loaderOtherUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
