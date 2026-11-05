import assert from 'node:assert';
import { mkdtempSync, writeFileSync } from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as FileSystem from 'effect/FileSystem';
import * as NodeFileSystem from '@effect/platform-node/NodeFileSystem';

// The real Node file system service, with only `readFile` used. The service is
// reached through every access form the acceptance set names.
const dir = mkdtempSync(path.join(os.tmpdir(), 'effect-service-shaking-'));
const file = path.join(dir, 'input.txt');
writeFileSync(file, 'hello from the node file system');

const decode = (bytes) => new TextDecoder().decode(bytes);

const viaYield = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem;
  return decode(yield* fs.readFile(file));
});

const viaMap = Effect.flatMap(
  Effect.map(FileSystem.FileSystem, (fs) => fs.readFile(file)),
  (read) => Effect.map(read, decode),
);

const viaContext = Effect.flatMap(Effect.context(), (context) =>
  Effect.map(Context.get(context, FileSystem.FileSystem).readFile(file), decode),
);

const viaService = Effect.flatMap(Effect.service(FileSystem.FileSystem), (fs) =>
  Effect.map(fs.readFile(file), decode),
);

const program = Effect.all([viaYield, viaMap, viaContext, viaService]);
const results = await Effect.runPromise(Effect.provide(program, NodeFileSystem.layer));
assert.deepStrictEqual(results, Array(4).fill('hello from the node file system'));
console.log(JSON.stringify(results));
