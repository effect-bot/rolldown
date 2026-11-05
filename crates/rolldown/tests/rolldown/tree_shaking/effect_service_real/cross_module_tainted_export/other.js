import * as Effect from "effect/Effect"

// Holds the Context of whichever Fiber last ran `remember`.
export let latest

export const remember = Effect.withFiber((fiber) =>
  Effect.sync(() => {
    latest = fiber.context
  })
)
