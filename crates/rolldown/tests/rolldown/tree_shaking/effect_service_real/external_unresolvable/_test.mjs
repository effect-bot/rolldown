import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The external module only appears now, next to the bundle. It reads a method
// chosen at run time.
fs.writeFileSync(
  path.join(import.meta.dirname, "dist/late.js"),
  'export const readWith = (store) => store[["lateStore", "Unused"].join("")]()\n'
)

// The program asserts its own output when imported.
await import("./dist/main.js")

// Run A's service keeps every method; run B's service loses its unread one.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["lateStoreUnused", "lateStoreUnusedHelper"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["lateOtherUnused", "lateOtherUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
