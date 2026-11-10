// Starts a child process in a session of its own (`detached: true`, `setsid` on Unix), waits until it runs, then hangs. The harness must stop the child.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

// The child writes a counter to `dist/beat` every 20 ms; the test checks that it stops.
const beat = fileURLToPath(new URL('./dist/beat', import.meta.url));
const child = spawn(
  process.execPath,
  [
    '-e',
    "const fs = require('fs'); let n = 0; setInterval(() => fs.writeFileSync(process.argv[1], String(++n)), 20)",
    beat,
  ],
  { stdio: 'ignore', detached: true },
);
child.unref();
while (!fs.existsSync(beat)) {
  await new Promise((resolve) => setTimeout(resolve, 10));
}
setTimeout(() => {}, 30_000);
