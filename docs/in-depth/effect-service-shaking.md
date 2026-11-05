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

Service keys are module-level declarations with a literal key string, as a `const` or a class whose body never mentions `this` or `super`:

```js
const Store = Context.Service('app/Store');
class Client extends Context.Service<Client, Shape>()('app/Client') {}
```

A class key's body may declare only static members with constant names, and none of them may redeclare what the key inherits from `Context.Service`: `of`, `key`, `use`, `useSync`, `context`, `pipe`, `toJSON`, `toString`, `name`, `length`, `prototype`, `caller`, `arguments`, `constructor`, the other `Object.prototype` members, or a `~` id. Any other class extending `Context.Service` turns the pass off. A class key's own static members (`OpenAiConfig.getOrUndefined`) may be read; other uses of the key follow the rules below.

Consumers read methods from `yield* Key` inside `Effect.gen` or `Effect.fn`:

```js
const program = Effect.gen(function* () {
  const store = yield* Store;
  return store.readOne();
});
```

Providers are `Layer.succeed(Key, { ... })`, or `Layer.effect(Key, effect)` where the effect is built from `Effect.gen`, `Effect.sync`, `Effect.succeed` and `Effect.acquireRelease`, and returns `Key.of({ ... })` or an object literal. A callback written as a named function expression is followed only when nothing references its name: through that name it can hand itself out and build another object from the same literal, so `Effect.sync(function inner() { exposed = inner; return { ... } })` keeps every method. Methods may be inline functions or constructor-local `const` functions:

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

Outside the effect package, a provider may also be built through functions, as generated clients are:

```js
// Generated.js
export const make = (http) => ({ listModels: () => ..., retrieveModel: () => ... });

// Client.js
import * as Generated from './Generated.js';
class Client extends Context.Service<Client, Shape>()('app/Client') {}
const make = Effect.fnUntraced(function* (options) {
  const http = yield* HttpClient.HttpClient;
  return Generated.make(http);
}, Effect.updateService(Headers.CurrentRedactedNames, append));
export const layer = (options) => Layer.effect(Client, make(options));

// main.js
const ClientLayer = layer({ apiUrl }).pipe(Layer.provide(HttpLayer));
```

The pass follows `Layer.effect(Key, make(args))` into a module-level `const` of the same module built by `Effect.fn` or `Effect.fnUntraced` from a generator function. Like a callback, that generator is not followed when it is a named function expression whose name is referenced. It follows `return value` and `return yield* effect` there, and then a call to a module-level function, in any module, that returns only object literals. That function must be a function declaration, an arrow or an anonymous function expression; a named function expression is not followed, because its own name can hand it out where the pass does not look. Those literals are provider objects only when every reference to the constructor and to the builder function is a call the pass followed into a provider; any other caller would get the same pruned object. The constructor's extra arguments must be `Effect.updateService`, `Effect.provideService`, `Effect.withSpan` or `Effect.annotateLogs` calls, which only change its context or tracing. A Layer returned from a module-level function is followed through each call of that function, and `layer.pipe(Layer.provide(a), ...)` with curried `Layer.provide`, `Layer.provideMerge` or `Layer.merge` is followed like the plain call.

## What it never removes

Demand is computed over code that can still run. Reads inside a removed method don't count. Anything that runs while a provider is constructed always counts. The pass deletes only object properties and unused constructor-local `const` functions. So service acquisition (`yield* Dep`), `acquireRelease` and its release, and construction-time calls keep running, in the same order. A property whose value is a binding is only removed when reading it cannot throw: the binding is a function declaration, a `var` or a parameter, or a `const`, `let`, class or catch binding declared earlier with no hoisted function declaration in between and not in a different case of the same `switch`. The read must also not run while the binding is still being initialised: inside the binding's own declaration (including an immediately invoked function or a class heritage there), inside the parameter list that declares it, inside the catch parameter pattern that declares it, or in the right-hand side of a `for…of` or `for…in` loop whose head declares it. An imported binding is always kept, because a module cycle can leave it uninitialised. This includes an import that resolves back to its own module, for example through a barrel that re-exports it.

## When it keeps everything

A service keeps every method when:
- its key is used in any way not listed above, such as `Key.use` or `Effect.map(Key, ...)`;
- the service value is passed anywhere, read with a computed key, or read through an inherited `Object.prototype` member;
- a kept provider method may read `this`, the object has getters, spreads or an explicit `__proto__`, or a release callback takes anything other than a single plain parameter. A method that may read `this` but is removed cannot run, so it keeps nothing;
- it is looked up other than by `yield*` with the key itself: `Context.get(context, Key)`, `Effect.updateService(Key, f)`, `Effect.provideService(Key, value)` and the other lookups listed below;
- its Layer may leave the bundle or be used in an unrecognised way.

Everything else in Effect is allowed and ignored: forks, joins, run options, `Effect.map`, `Effect.all`, loggers, Schema, Stream, retries and timeouts. Holding a Fiber or a Context is fine; the contract below rules out reflecting on them.

The pass is turned off for the whole build when user code:
- exports anything from the entry module, or imports an external module other than a Node builtin;
- loads code outside the module graph: `eval`, `Function`, `require`, a `.constructor` read, dynamic `import()`, `process.getBuiltinModule`, `module.createRequire`, or any use other than `typeof` of a binding imported from `vm`, `module`, `inspector` or `repl`, including through re-exports. A member name counts when the compiler can work it out: a string literal, a template literal, or `+` over them and over `const` bindings holding one, such as `fn["con" + "structor"]`. The same applies to the key argument of `Reflect.get` on the global `Reflect`, bare or through the global object (`globalThis.Reflect`), such as `Reflect.get(fn, "constructor")`, and of any method named `getOwnPropertyDescriptor`. That method is matched on any receiver as a conservative fallback, since aliases of `Object` are not tracked, so a user method with the same name is treated the same way. Calling one of them with a spread before the key, using them other than by calling them, or using the global `Reflect` other than through a member read or `typeof` (for example `const { get } = globalThis.Reflect`) also turns the pass off. A name built at run time is not checked (see A2);
- uses a host value that leads to code loaders (`process`, `globalThis`, `global`, `self`, `window`, `module`, or a namespace or default import of `node:process`) other than through constant member names, calls and `typeof`: for example `process[name]` or `const p = process`;
- looks a service up other than by `yield*` by something other than a declared key: `Context.get`, `getUnsafe`, `getOption`, `getOrElse`, `getOrUndefined`, `pick`, `omit`, `Effect.service`, `serviceOption`, `updateService`, `updateServiceScoped`, and `Layer`, `Stream` and `Channel.updateService`, with a key argument such as `{ key: "app/Store" }` or `cond ? A : B`;
- calls `Context.getOrUndefinedUnsafe` or `Context.makeUnsafe`, which work on key strings;
- uses `Context.Service` other than to declare a module-level key with a literal string, or writes to a key's members;
- modifies a builtin prototype or uses one as a value (`Map.prototype.get = ...`).

## Contract

The POC targets closed Effect applications. It relies on these assumptions, approved for the POC; programs that break them can be miscompiled instead of keeping methods:

- **A1.** User code does not read, enumerate, serialise, format or mutate Effect runtime objects (Contexts, Fibers, Exits, Layers, Effect descriptions and their prototypes), and does not modify builtin prototypes. Services are reached only through keyed lookups.
- **A2.** The entry module exports nothing but primitive constants, and external imports are host builtins. Operations that load code (`eval`, `Function`, `require`, `vm`, `module.createRequire`) turn the pass off when the compiler can name them, including through the reflective reads `Reflect.get` and `getOwnPropertyDescriptor` with a constant key. User code does not reach a code loader through a member name built at run time, such as `(() => 0)[["con", "structor"].join("")]`. This is assumed, not checked: the pass does not detect it, and a program that does it can have methods removed and then fail at run time. Excluding it keeps ordinary computed reads on values other than services and host values, such as `settings[key]` or `fibers[i]`, from turning the pass off.
- **A3.** Effect's own modules read properties of user service values only through keyed lookups, and enumerate Context entries only under their own key prefixes. User keys don't use those prefixes.
- **A4.** Services are provided and read through the same bundled copy of `effect`.

Some breaches are cheap to detect and turn the pass off rather than relying on A1: the string-keyed Context APIs (`Context.getOrUndefinedUnsafe`, `Context.makeUnsafe`), keyed lookups other than `yield*` by an untracked key, `Context.Service` used other than to declare a key, and writes to builtin prototypes. Every other Effect API is allowed. A consumer or provider shape the pass doesn't model keeps every method of the key involved.

## Inspecting the result

Every reason that turns the pass off is reported once per site as an `EFFECT_SERVICE_SHAKING` warning, with `at <line>:<col>` and the module. `checks.effectServiceShaking: false` silences them.

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

`reflection_clean_data`, `provenance_json_clean`, `clean_builtin_callback`, `unused_host_import`, `provenance_success_channel` and `provenance_numeric_index` enumerate, serialise and transform user data, pass Effects through `Effect.map` and `Effect.all`, or import a Node module without calling it, and must still remove `writeOne`. `forged_key`, `builtin_prototype_write`, `node_prefixed_vm` and `host_module_loader` reach a service through a lookup by an untracked key, a patched builtin or code outside the bundle, and check that every method stays. The stub fixtures in `tree_shaking/effect_service` cover key identity, service escapes and writes, unsupported consumer shapes, provider shapes, lifecycle and the A2 boundary.

`tree_shaking/effect_generated_client/openai` provides the generated client of `@effect/ai-openai` 4.1.2 through its service and calls only `retrieveModel` against a mock HTTP client. The pass keeps `retrieveModel` and its response schema and removes the other 378 methods, `httpClient` and the schemas only they used: the bundle goes from 2,068,645 to 532,916 bytes. It is not snapshotted and runs separately (`-- effect_generated_client`), because bundling it takes a few seconds.

Once the `effect-verify` profile is built, `cargo test --profile effect-verify -p rolldown --test integration -- effect_service` runs every fixture in about 4 seconds. CI fails the step if it takes longer than 10 seconds.

The analysis costs about 1.1–1.2x of an unoptimized build on Effect programs of 0.4–1.6 MB.

## Limitations

This is a proof of concept for the generator/constructor base case and generated clients. The `NodeFileSystem` provider builds its object by spreading a parameter, keyed access other than `yield*` keeps the key's methods (or turns the pass off for an untracked key), combinator consumers keep their key, constructors with other pipeables are not followed, and library builds with exports are not optimized. Programs that break the contract can be miscompiled instead of keeping methods.
