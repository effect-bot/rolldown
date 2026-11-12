// No import here reaches effect, so the pass treats this module as a hole.
export function nameIt(o) {
  o.name = 'test/AttributionHoleFirst';
}
