// Twenty-four unused arrow functions, each calling the one numbered before it,
// declared evens first and then odds in reverse. Nothing calls any of them, so
// dead-code elimination should drop all of them whatever their order.
const link0 = () => unreachable();
const link2 = () => link1();
const link4 = () => link3();
const link6 = () => link5();
const link8 = () => link7();
const link10 = () => link9();
const link12 = () => link11();
const link14 = () => link13();
const link16 = () => link15();
const link18 = () => link17();
const link20 = () => link19();
const link22 = () => link21();
const link23 = () => link22();
const link21 = () => link20();
const link19 = () => link18();
const link17 = () => link16();
const link15 = () => link14();
const link13 = () => link12();
const link11 = () => link10();
const link9 = () => link8();
const link7 = () => link6();
const link5 = () => link4();
const link3 = () => link2();
const link1 = () => link0();

export const live = 'live';
