import assert from "node:assert"
import fs from "node:fs"
import path from "node:path"

// The program asserts its own output when imported.
await import("./dist/main.js")

// Only `createCustomer` is read from `Billing`: the other two methods and the
// operation spec only they use are removed, while the used operation stays.
const code = fs.readFileSync(path.join(import.meta.dirname, "dist/main.js"), "utf8")
assert.match(code, /`CreateCustomerRequest`/, "CreateCustomer should be kept")
for (const name of ["retrieveCustomer", "createCheckout"]) {
  assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${name} should be removed`)
}
assert.doesNotMatch(code, /`CreateCheckoutSessionRequest`/, "CreateCheckoutSession should be removed")
