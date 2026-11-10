// An ordinary application: a CLI that validates a YAML config, matches files
// against globs, compares versions and uses one small Effect service for the
// store it reads. Most of the code and the packages are plain JavaScript.
import { Command } from 'commander';
import { z } from 'zod';
import YAML from 'yaml';
import { nanoid } from 'nanoid';
import semver from 'semver';
import picomatch from 'picomatch';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';

const ConfigSchema = z.object({
  name: z.string(),
  version: z.string(),
  include: z.array(z.string()),
  minEngine: z.string().optional(),
});

const sampleConfig = `
name: demo
version: 1.4.2
include:
  - "src/**/*.js"
  - "lib/*.mjs"
minEngine: ">=18.0.0"
`;

// The one Effect service: a key-value store with three methods, of which the
// program only ever calls `get`. Each body is a named function, so the test can
// tell this provider's methods from any other `put` or `del` in the bundle.
class Store extends Context.Service()('app/Store') {}

const StoreLive = Layer.succeed(Store, {
  get: (key) =>
    Effect.sync(function storeGetBody() {
      return memory.get(key);
    }),
  put: (key, value) =>
    Effect.sync(function storePutBody() {
      memory.set(key, value);
    }),
  del: (key) =>
    Effect.sync(function storeDelBody() {
      return memory.delete(key);
    }),
});
const memory = new Map([['greeting', 'hello']]);

const program = Effect.gen(function* () {
  const store = yield* Store;
  const greeting = yield* store.get('greeting');
  return `${greeting} ${nanoid(6).length}`;
});

const cli = new Command();
cli
  .name('ordinary')
  .description('an ordinary app')
  .option('-c, --config <text>', 'yaml config', sampleConfig)
  .action(async (opts) => {
    const config = ConfigSchema.parse(YAML.parse(opts.config));
    const isMatch = picomatch(config.include);
    const files = ['src/index.js', 'lib/util.mjs', 'docs/readme.md'];
    const matched = files.filter((f) => isMatch(f));
    const engineOk = semver.satisfies('20.1.0', config.minEngine ?? '*');
    const result = await Effect.runPromise(Effect.provide(program, StoreLive));
    console.log(
      JSON.stringify({
        name: config.name,
        version: semver.clean(config.version),
        matched,
        engineOk,
        result,
      }),
    );
  });

cli.parseAsync(['node', 'ordinary']);
