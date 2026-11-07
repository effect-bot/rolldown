// Twelve unused arrow functions, each calling the one declared before it.
// Nothing calls any of them, so dead-code elimination should drop all twelve.
const link0 = () => console.log('unreachable');
const link1 = () => link0();
const link2 = () => link1();
const link3 = () => link2();
const link4 = () => link3();
const link5 = () => link4();
const link6 = () => link5();
const link7 = () => link6();
const link8 = () => link7();
const link9 = () => link8();
const link10 = () => link9();
const link11 = () => link10();

export const live = 'live';
