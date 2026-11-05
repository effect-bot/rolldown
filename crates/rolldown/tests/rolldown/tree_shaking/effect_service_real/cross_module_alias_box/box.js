// Serializes `box`, which another module fills.
export const box = {}

const replacer = (_key, value) => (typeof value === "function" ? "fn" : value)

export const snapshot = () => (JSON.stringify(box, replacer).includes('"unused"') ? "unused" : "absent")
