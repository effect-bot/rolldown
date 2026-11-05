import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Operations are separate module-level exports, so Rolldown's own tree shaking
// keeps only the one used, with or without the service pass.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
assert.match(code, /`CreateCustomerRequest`/, "CreateCustomer should be kept")
for (const name of ["CreateAccountRequest", "CreateInvoiceRequest"]) {
  assert.doesNotMatch(code, new RegExp(`\`${name}\``), `${name} should be removed`)
}
