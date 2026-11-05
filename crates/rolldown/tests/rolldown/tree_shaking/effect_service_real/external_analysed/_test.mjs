import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The external module is deployed next to the bundle, unchanged.
fs.mkdirSync(path.join(import.meta.dirname, "dist/ext"), { recursive: true })
fs.copyFileSync(path.join(import.meta.dirname, "ext/reader.js"), path.join(import.meta.dirname, "dist/ext/reader.js"))

// The program asserts its own output when imported.
await import("./dist/main.js")

// The external's read is kept; the method it never reads is removed.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["readOne"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["externalUnused", "externalUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
