// Imports nothing from `effect`: a module hole. It keeps what it is given and calls none of it.
const kept = [];
export function keep(...values) {
  kept.push(values.length);
}
