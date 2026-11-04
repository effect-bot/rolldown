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

Holding a Fiber or a Context is fine, and so is any Effect API. A Context only hands out a service through a lookup by key, and the pass tracks every key. The other ways to reach a service are handled in user code (anything outside the Effect and `@effect/platform-node` packages) as follows.

These always turn the pass off for the whole build:
- reading a Context internal by name: `mapUnsafe`, `_flat`, `base`, `overlay`, `cacheRoot` or `toJSON`, statically, by destructuring, through a constant computed name (`context["map" + "Unsafe"]`, a `const` holding the name) or as a constant argument of `Object` or `Reflect` functions;
- `__lookupGetter__`, `__lookupSetter__`, `__defineGetter__` and `__defineSetter__`;
- `Context.getOrUndefinedUnsafe` and `Context.makeUnsafe`, which work on key strings;
- running code outside the module graph: `eval`, `Function`, `require`, any `.constructor` read, `with`, dynamic `import()`, the `vm`, `module`, `inspector` and `repl` builtins (with or without `node:`), and computed reads on the global object by a non-constant name;
- exports from the entry module, external imports from user code other than Node builtins, and a `Context.Service` call that does not declare a module-level key with a literal string.

Everything else depends on provenance: whether the value involved may hold an Effect runtime object (a Context, a Fiber, a service, or anything that leads to one). A value is clean only when the pass can show it holds none:
- literals, strings, numbers, functions, and arrays and objects built from clean values;
- results of ECMAScript builtins applied to clean values (`JSON.parse`, `structuredClone`, `Object.keys`, string and array methods);
- `const`, `let` and `var` bindings whose every assignment is clean, and parameters of local functions that are only called directly, with clean arguments;
- the success of `Effect.succeed`, `sync`, `gen`, `map`, `flatMap`, `tap`, `provide` and `all` over clean values, as seen by `yield*`, `Effect.runPromise` and the callbacks of `map`, `flatMap` and `tap`;
- the results of user service methods whose every provider returns a clean value.

Anything else may hold a runtime object: the result of any other Effect call, parameters of callbacks Effect calls, imports, `this`, caught errors and the global object. Writing such a value into any object or collection makes every object and array literal in user code unclean too.

When an operand may hold a runtime object, these turn the pass off:
- enumeration and reflection: `Object.keys`, `values`, `entries`, `getOwnPropertyNames`, `getOwnPropertyDescriptor(s)`, `getOwnPropertySymbols`, `hasOwn`, any `Reflect` function, `for...in` and `structuredClone`;
- member reads by a non-constant name, except numeric indexes such as `fibers[i]` in a counted loop;
- serialization and formatting: `JSON.stringify`, template literals, `String()`, `+`, loose comparisons with a non-number, `toString`, `join`, string methods that coerce their arguments, `console`, `util`, `throw`, `Error` constructors and `super`, and the Effect APIs that format (`Effect.fail`, `die`, `log*`, `Console`, `Inspectable`, `Cause`, `Logger`).

These builtins must be called in place. Aliasing them (`const values = Object.values`), passing them around or using the global object as a value turns the pass off.

A static read on a value that may hold a runtime object (`value.unused()`, `const { unused } = value`, `"unused" in value`) keeps that method on every service, instead of turning the pass off.

## Assumptions

The pass assumes that:
- the Effect and `@effect/platform-node` packages don't enumerate, format or call service objects on behalf of user code other than through the APIs listed above, and don't store runtime objects into objects that user code created. This is not checked;
- bundled code doesn't modify builtins such as `Map.prototype` in ways the provenance rules don't see;
- printed output of user data doesn't change. Printing a value that may hold a runtime object turns the pass off.

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

`reflection_clean_data`, `provenance_success_channel`, `provenance_numeric_index` and `provenance_json_clean` enumerate, index and serialize user data and still remove `writeOne`. The other fixtures in `effect_service_real` reach a service through a Context, a forged key, a patched builtin, `Inspectable` or code outside the bundle, and check that every method stays.

Once the `effect-verify` profile is built, `cargo test --profile effect-verify -p rolldown --test integration -- effect_service` runs every fixture in about 4 seconds. CI fails the step if it takes longer than 10 seconds.

The analysis costs about 1.1–1.2x of an unoptimized build on Effect programs of 0.4–1.6 MB.

## Limitations

This is a proof of concept for the generator/constructor base case. The `NodeFileSystem` provider builds its object by spreading a parameter, keyed access other than `yield*`, combinator consumers and library builds with exports are not optimized.
