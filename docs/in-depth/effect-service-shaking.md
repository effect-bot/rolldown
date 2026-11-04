# Effect Service Shaking (experimental)

An [Effect](https://effect.website) service is an object of methods behind a key. Rolldown keeps a whole object literal when any part of it is used, so a program that reads one method of a service still bundles every other method and every helper those methods reference.

`experimental.effectServiceShaking` removes the methods nothing in the bundle can read. It is off by default and limited to a small set of Effect patterns. Anything else keeps every method, or turns the pass off for the whole build.

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

Holding a Fiber or a Context is fine, and so is any Effect API. A Context only hands out a service through a lookup by key, and the pass tracks every key. The other ways to get a service out of a Context are sinks. When user code (anything outside the Effect and `@effect/platform-node` packages) reaches a sink, the pass is turned off for the whole build.

These sinks apply in every mode:
- reading a Context internal by name: `mapUnsafe`, `_flat`, `base`, `overlay`, `cacheRoot` or `toJSON`. Static reads, destructuring, constant computed names (`context["map" + "Unsafe"]`, a `const` holding the name) and constant names passed to `Object` or `Reflect` functions all count, on any object;
- `Context.getOrUndefinedUnsafe` and `Context.makeUnsafe`, which work on key strings;
- running code outside the module graph: `eval`, `Function`, `require`, any `.constructor` read, `with`, dynamic `import()`, the `vm`, `module`, `inspector` and `repl` builtins, and computed reads on the global object by a non-constant name;
- exports from the entry module, and external imports other than Node builtins;
- a `Context.Service` call that does not declare a module-level key with a literal string, and writes to a key's members.

These sinks find a service without naming it:
- enumeration: `Object.keys`, `values`, `entries`, `getOwnPropertyNames`, `getOwnPropertyDescriptors` and `getOwnPropertySymbols` (also through aliases, `.call` and destructuring), `Object` used as a value, any use of `Reflect`, `for...in` and `structuredClone`;
- member reads by a non-constant name. Numeric indexes, `Symbol.iterator` and `Symbol.asyncIterator` are exempt;
- `JSON.stringify` of anything but a constant. A replacer sees every service of a Context through `toJSON`, and the output includes literal properties the pass removes.

They make the default mode safe. They also turn the pass off for most real programs, since plain `arr[i]` and `Object.keys` are common.

## Assuming no reflection

`experimental.effectServiceShakingAssumeNoReflection` turns off only the second list of sinks:

```js
export default {
  experimental: {
    effectServiceShaking: true,
    effectServiceShakingAssumeNoReflection: true,
  },
};
```

With it, you promise that user code never enumerates, indexes by a non-constant name or serializes an Effect runtime object (a Context, a Fiber, a Layer, a service, or anything reached from them), or indexes the global object through an alias. A program that breaks the promise can read a removed method and fail at runtime. For example, `Object.values(context)` finds the service map and `context[["map", "Unsafe"].join("")]` builds an internal name the pass can't evaluate. Reads of Context internals by a constant name still turn the pass off.

## Assumptions

In both modes the pass assumes that:
- the Effect and `@effect/platform-node` packages don't inspect service values or expose them other than through a keyed lookup. This is not checked;
- bundled code doesn't modify built-ins such as `Map.prototype`;
- service lookups use keys created with `Context.Service`. A hand-made key object such as `{ key: "app/Store" }` passed to `Context.get` or `Effect.service` is not tracked;
- printed output doesn't matter. `console.log(context)` lists the services and their properties, so removed methods disappear from it.

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
| `run_options` | `writeOne` and its helper, with `RunOptions.onFiberStart` | the program result |
| `retained_fiber` | `writeOne` and its helper, with a forked Fiber kept after it ends | the program result |

The `internals_destructuring`, `enumeration`, `enumeration_alias`, `dynamic_name` and `json_literal` fixtures read a service through a sink and check that nothing is removed.

The analysis costs about 1.1–1.2x of an unoptimized build on Effect programs of 0.4–1.6 MB.

## Limitations

This is a proof of concept for the generator/constructor base case. The `NodeFileSystem` provider builds its object by spreading a parameter, keyed access other than `yield*`, combinator consumers and library builds with exports are not optimized.
