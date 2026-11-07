// Thirteen unused arrow functions, each calling the one declared before it.
// Every initializer but the first calls tick before it yields its arrow, so
// dead-code elimination may drop each arrow but must still call tick, in order.
export const ticks = [];
function tick(n) {
  ticks.push(n);
}
const link0 = () => unreachable();
const link1 = (tick(1), () => link0());
const link2 = (tick(2), () => link1());
const link3 = (tick(3), () => link2());
const link4 = (tick(4), () => link3());
const link5 = (tick(5), () => link4());
const link6 = (tick(6), () => link5());
const link7 = (tick(7), () => link6());
const link8 = (tick(8), () => link7());
const link9 = (tick(9), () => link8());
const link10 = (tick(10), () => link9());
const link11 = (tick(11), () => link10());
const link12 = (tick(12), () => link11());
