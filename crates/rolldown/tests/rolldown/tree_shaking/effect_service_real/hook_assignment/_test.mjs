import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// The clean `toJSON` assignment must keep every method: `unused` stays.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
assert.match(code, /\bunused\b/, "unused should be kept")
