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

Holding a Fiber or a Context is fine. A Context only hands out a service through a lookup by key, and the pass tracks every key. Every other way to reach a service goes through a value that may hold an Effect runtime object (a Context, a Fiber, a service, or anything that leads to one). User code is anything outside the Effect and `@effect/platform-node` packages. The pass tracks two facts for every value in user code:
- its value taint: whether it may hold a runtime object;
- for Effects, its success taint: whether the value it succeeds with may hold one.

Facts start clean and are solved as a least fixpoint across keys and modules. A value is clean when it is one of:
- a literal, or an array or object built from clean values;
- a function whose captured bindings are all clean and which doesn't read `this`, `arguments` or the global object;
- the result of an ECMAScript builtin whose arguments, and the values its callbacks return, are clean;
- the result of a method called on an array, a string or a builtin collection, under the same conditions.

The methods named `toJSON`, `valueOf`, `toString` and `toLocaleString` and the symbol hooks run implicitly, so what they return counts toward the object. Parameters are clean only for local functions called directly with clean arguments. A consumer's method result is the join of that method's returns over every provider of the key, and an import is the exporting module's fact.

Effect operations are followed only in these exact shapes; any other trusted call takes the fallback:

| row | shape |
| --- | --- |
| 1 | `Context.Service("literal")` declaring a module-level key |
| 2 | `Key.of(value)` |
| 3, 4 | `Layer.succeed(Key, value)`, `Layer.effect(Key, effect)` or `Layer.effect(Key)(effect)`, with `effect` from row 5 or 7 |
| 5, 6 | `Effect.gen(function* () {...})`, `Effect.sync(() => ...)` |
| 7 | `Effect.acquireRelease(acquire, release)` |
| 8, 9 | `Layer.mergeAll(...)`, `Layer.provide(a, b)`, `Effect.provide(effect, layer)` |
| 10 | `Effect.runPromise(effect)` or `Effect.runPromise(effect, { onFiberStart })` |
| 11 | `assert.strictEqual(a, b)`, `assert.deepStrictEqual(a, b)`, `assert.ok(x)` from `node:assert` |
| 12, 13 | `Effect.forkChild(effect)`, `Fiber.join(fiber)` |

The summaries are checked against effect 4.1.2. Any other resolved `effect` version turns the pass off.

The heap bit is one build-wide fact. It is set when user code:
- writes into an object that may be a runtime object or whose provenance is unresolved;
- stores a value that may hold one;
- assigns an implicitly invoked hook;
- writes by a non-constant name;
- installs accessors or prototypes (`Object.defineProperty`, `Object.assign`, `setPrototypeOf`, ...);
- calls an Effect API without a row;
- passes a runtime value to a callee the analysis doesn't follow.

While it is set, every allocation, method result, join result and awaited run result may hold a runtime object.

The pass turns off for the whole build when:
- user code reads a Context internal by name (`mapUnsafe`, `_flat`, `base`, `overlay`, `cacheRoot`, `toJSON`), uses `__lookupGetter__` and friends, `Context.getOrUndefinedUnsafe` or `Context.makeUnsafe`;
- user code runs code outside the module graph: `eval`, `Function`, `require`, `.constructor`, `with`, dynamic `import()`;
- the entry module has exports, user code imports an external module other than a Node builtin, or a `Context.Service` call declares no module-level key with a literal string;
- user code calls a host operation without a summary (any Node builtin or non-ECMAScript global other than the assertions above), or uses one as a value;
- an Effect API without a row receives a value that may hold a runtime object;
- a value that may hold a runtime object reaches enumeration (`Object.keys`, `Reflect`, `for...in`, `structuredClone`, ...), a member read by a non-constant name (numeric indexes aside), serialization or formatting (`JSON.stringify`, templates, `String()`, `+`, loose comparisons, `toString`, `join`, string methods, `throw`, `Error`, the assertions).

A static read on a value that may hold a runtime object keeps that method on every service instead.

## Contract

The POC targets closed Effect applications. It relies on these assumptions, approved for the POC; programs that break them can be miscompiled instead of keeping methods:

- **A1.** User code does not read, enumerate, serialise, format or mutate Effect runtime objects (Contexts, Fibers, Exits, Layers, Effect descriptions and their prototypes), and does not modify builtin prototypes. Services are reached only through keyed lookups.
- **A2.** The entry module exports nothing but primitive constants, and external imports are host builtins. Operations that load code (`eval`, `Function`, `require`, `vm`, `module.createRequire`) turn the pass off.
- **A3.** Effect's own modules read properties of user service values only through keyed lookups, and enumerate Context entries only under their own key prefixes. User keys don't use those prefixes.
- **A4.** Services are provided and read through the same bundled copy of `effect`.

Some breaches are cheap to detect and turn the pass off rather than relying on A1: the string-keyed Context APIs (`Context.getOrUndefinedUnsafe`, `Context.makeUnsafe`), `Context.Service` used other than to declare a key, and writes to builtin prototypes. Every other Effect API is allowed. A consumer or provider shape the pass doesn't model keeps every method of the key involved.

## Inspecting the result

Every reason that turns the pass off, and every reason that sets the heap bit, is reported once per tag and site as an `EFFECT_SERVICE_SHAKING` warning. The message contains `[<tag>]` and `at <line>:<col>`. `checks.effectServiceShaking: false` silences them.

Set `ROLLDOWN_EFFECT_SERVICE_SHAKING_REPORT=1` to print, for each build:
- the reasons above;
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

`reflection_clean_data`, `provenance_json_clean`, `clean_builtin_callback`, `unused_host_import`, `provenance_success_channel` and `provenance_numeric_index` enumerate, serialise and transform user data, pass Effects through `Effect.map` and `Effect.all`, or import a Node module without calling it, and must still remove `writeOne`. `forged_key`, `builtin_prototype_write`, `node_prefixed_vm` and `host_module_loader` reach a service through a lookup by an untracked key, a patched builtin or code outside the bundle, and check that every method stays. The stub fixtures in `tree_shaking/effect_service` cover key identity, service escapes and writes, unsupported consumer shapes, provider shapes, lifecycle and the A2 boundary.

Once the `effect-verify` profile is built, `cargo test --profile effect-verify -p rolldown --test integration -- effect_service` runs every fixture in about 4 seconds. CI fails the step if it takes longer than 10 seconds.

The analysis costs about 1.1–1.2x of an unoptimized build on Effect programs of 0.4–1.6 MB.

## Limitations

This is a proof of concept for the generator/constructor base case. The `NodeFileSystem` provider builds its object by spreading a parameter, keyed access other than `yield*`, combinator consumers and library builds with exports are not optimized.
