// No import here reaches effect, so the pass treats this module as a hole.
export function forward() {
  return arguments[1](arguments[0]);
}
