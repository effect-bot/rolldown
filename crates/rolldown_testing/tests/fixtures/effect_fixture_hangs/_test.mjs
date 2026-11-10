// Hangs, with a child process of its own. The harness must stop both.
import { spawn } from 'node:child_process';
import fs from 'node:fs';

const sleeper = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 30_000)'], {
  stdio: 'ignore',
});
fs.writeFileSync(new URL('./dist/sleeper.pid', import.meta.url), String(sleeper.pid));
setTimeout(() => {}, 30_000);
