import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported. The importer then reads
// a method of the exported service by a name built at run time.
const { withStore } = await import("./dist/main.js")
const name = ["libraryStore", "Unused"].join("")
assert.deepStrictEqual(await withStore((store) => [store.readStore(), store[name]()]), ["s1", "s2"])

// The exported service keeps every method; the internal service loses its unread one.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
for (const name of ["libraryStoreUnused", "libraryStoreUnusedHelper"]) {
  assert.match(code, new RegExp(`\\b${name}\\b`), `${name} should be kept`)
}
for (const name of ["libraryInternalUnused", "libraryInternalUnusedHelper"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
