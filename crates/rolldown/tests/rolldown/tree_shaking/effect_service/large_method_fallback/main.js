import assert from 'node:assert';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

// A method that closes over `this` and is too large to clone per receiver keeps
// one shared body for both receivers; the closure reads the service through `this`.
const proto = {
  describe() {
    const read = () => this.target.largeMethodUsed();
    const out = {};
    out.f0 = this.base + 0;
    out.f1 = this.base + 1;
    out.f2 = this.base + 2;
    out.f3 = this.base + 3;
    out.f4 = this.base + 4;
    out.f5 = this.base + 5;
    out.f6 = this.base + 6;
    out.f7 = this.base + 7;
    out.f8 = this.base + 8;
    out.f9 = this.base + 9;
    out.f10 = this.base + 10;
    out.f11 = this.base + 11;
    out.f12 = this.base + 12;
    out.f13 = this.base + 13;
    out.f14 = this.base + 14;
    out.f15 = this.base + 15;
    out.f16 = this.base + 16;
    out.f17 = this.base + 17;
    out.f18 = this.base + 18;
    out.f19 = this.base + 19;
    out.f20 = this.base + 20;
    out.f21 = this.base + 21;
    out.f22 = this.base + 22;
    out.f23 = this.base + 23;
    out.f24 = this.base + 24;
    out.f25 = this.base + 25;
    out.f26 = this.base + 26;
    out.f27 = this.base + 27;
    out.f28 = this.base + 28;
    out.f29 = this.base + 29;
    out.f30 = this.base + 30;
    out.f31 = this.base + 31;
    out.f32 = this.base + 32;
    out.f33 = this.base + 33;
    out.f34 = this.base + 34;
    out.f35 = this.base + 35;
    out.f36 = this.base + 36;
    out.f37 = this.base + 37;
    out.f38 = this.base + 38;
    out.f39 = this.base + 39;
    out.f40 = this.base + 40;
    out.f41 = this.base + 41;
    out.f42 = this.base + 42;
    out.f43 = this.base + 43;
    out.f44 = this.base + 44;
    out.f45 = this.base + 45;
    out.f46 = this.base + 46;
    out.f47 = this.base + 47;
    out.f48 = this.base + 48;
    out.f49 = this.base + 49;
    out.f50 = this.base + 50;
    out.f51 = this.base + 51;
    out.f52 = this.base + 52;
    out.f53 = this.base + 53;
    out.f54 = this.base + 54;
    out.f55 = this.base + 55;
    out.f56 = this.base + 56;
    out.f57 = this.base + 57;
    out.f58 = this.base + 58;
    out.f59 = this.base + 59;
    out.f60 = this.base + 60;
    out.f61 = this.base + 61;
    out.f62 = this.base + 62;
    out.f63 = this.base + 63;
    out.f64 = this.base + 64;
    out.f65 = this.base + 65;
    out.f66 = this.base + 66;
    out.f67 = this.base + 67;
    out.f68 = this.base + 68;
    out.f69 = this.base + 69;
    out.f70 = this.base + 70;
    out.f71 = this.base + 71;
    out.f72 = this.base + 72;
    out.f73 = this.base + 73;
    out.f74 = this.base + 74;
    out.f75 = this.base + 75;
    out.f76 = this.base + 76;
    out.f77 = this.base + 77;
    out.f78 = this.base + 78;
    out.f79 = this.base + 79;
    out.f80 = this.base + 80;
    out.f81 = this.base + 81;
    out.f82 = this.base + 82;
    out.f83 = this.base + 83;
    out.f84 = this.base + 84;
    out.f85 = this.base + 85;
    out.f86 = this.base + 86;
    out.f87 = this.base + 87;
    out.f88 = this.base + 88;
    out.f89 = this.base + 89;
    out.f90 = this.base + 90;
    out.f91 = this.base + 91;
    out.f92 = this.base + 92;
    out.f93 = this.base + 93;
    out.f94 = this.base + 94;
    out.f95 = this.base + 95;
    out.f96 = this.base + 96;
    out.f97 = this.base + 97;
    out.f98 = this.base + 98;
    out.f99 = this.base + 99;
    return [read(), Object.keys(out).length];
  },
};
const plain = { largeMethodUsed: () => 'plain' };

const Store = Context.Service('test/LargeMethodStore');
const StoreLayer = Layer.succeed(
  Store,
  Store.of({ largeMethodUsed: () => 'used', unusedLargeMethod: () => 'unused' }),
);

const program = Effect.gen(function* () {
  const held = { __proto__: proto, base: 1, target: yield* Store };
  const other = { __proto__: proto, base: 2, target: plain };
  return [held.describe(), other.describe()];
});
assert.deepStrictEqual(await Effect.runPromise(Effect.provide(program, StoreLayer)), [
  ['used', 100],
  ['plain', 100],
]);
