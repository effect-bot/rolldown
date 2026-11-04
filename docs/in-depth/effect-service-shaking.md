# Effect Service Shaking (experimental)

An [Effect](https://effect.website) service is an object of methods behind a key. Rolldown keeps a whole object literal when any part of it is used, so a program that reads one method of a service still bundles every other method and every helper those methods reference.

`experimental.effectServiceShaking` removes the methods nothing in the bundle can read. It is off by default and limited to a small, fully checked set of Effect patterns. Anything else keeps every method, or turns the pass off for the whole build.

```js
export default {
  input: './main.js',
  platform: 'node',
  experimental: {
    effectServiceShaking: true,
  },
};
```

## What it handles

Service keys are module-level declarations with a literal key string:

```js
const Store = Context.Service('app/Store');
```

Consumers read methods from `yield* Key` inside `Effect.gen` or `Effect.fn`:

```js
const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.readOne();
});
```

Providers are `Layer.succeed(Key, { ... })`, or `Layer.effect(Key, effect)` where the effect is built from `Effect.gen`, `Effect.sync`, `Effect.succeed` and `Effect.acquireRelease`, and returns `Key.of({ ... })` or an object literal. Methods may be inline functions or constructor-local `const` functions:

```js
const StoreLayer = Layer.effect(
  Store,
  Effect.gen(function* () {
    const dependency = yield* Dependency;
    const readOne = () => dependency.read();
    const writeOne = () => dependency.write();
    return Store.of({ readOne, writeOne });
  }),
);
```

With only `readOne` read anywhere, the pass removes `writeOne` from the provider. Because `writeOne` was the only reader of `dependency.write`, it also removes `write` from the `Dependency` provider. Rolldown's tree shaking then drops the helpers that only those methods used.

## What it never removes

Demand is computed over code that can still run. Reads inside a removed method don't count. Anything that runs while a provider is constructed always counts. The pass deletes only object properties and unused constructor-local `const` functions. So service acquisition (`yield* Dep`), `acquireRelease` and its release, and construction-time calls keep running, in the same order.

## When it keeps everything

A service keeps every method when:
- its key is used in any way not listed above, such as `Key.use`, `Effect.map(Key, ...)`, `Context.get` or `updateService`;
- the service value is passed anywhere, read with a computed key, or read through an inherited `Object.prototype` member;
- a provider method may read `this`, the object has getters, spreads or an explicit `__proto__`, or a release callback takes anything other than a single plain parameter;
- its Layer may leave the bundle or be used in an unrecognised way.

The pass is turned off for the whole build when:
- the entry module has any export;
- there is an external import other than a Node builtin;
- user code uses `globalThis`, `Reflect`, `Proxy`, `eval`, `Function`, `Symbol.for` or symbol-revealing `Object` APIs, or uses a module namespace as a value;
- user code uses an Effect API outside an audited list. Run functions are only allowed without options, and `runFork` only when its Fiber is discarded.

These rules exist because a running Fiber, and through it any Context, is reachable from user code by those routes. Any service can be retrieved from a Context by its key string, which would bypass the analysis.

The Effect and `@effect/platform-*` packages are trusted not to inspect service values or hand out a Context or Fiber outside the audited APIs. This is assumed, not checked.

## Inspecting the result

Set `ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT=1` to print, for each build:
- why the pass was turned off, if it was;
- which keys kept every method, and why;
- the demand per key;
- every removed property and local.

## Results

The `tree_shaking/effect_service_real` fixtures run against the published `effect` package:

| fixture | removed | preserved |
| --- | --- | --- |
| `lifecycle` | `writeOne`, the dependency's `write` and the audit service's `record`, plus their unique helpers | acquisition, construction-time calls and release, in order |
| `cross_module` | `writeOne` and its helper from another module | the live helper |

The analysis costs about 1.1–1.2x of an unoptimized build on Effect programs of 0.4–1.6 MB.

## Limitations

This is a proof of concept for the generator/constructor base case. The `NodeFileSystem` provider builds its object by spreading a parameter, direct Context access, combinator consumers and library builds with exports are not optimized.
