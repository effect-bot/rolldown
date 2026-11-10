// Starts a child process and finishes without waiting for it. The harness must stop the child.
import { spawn } from 'node:child_process';
import fs from 'node:fs';

const sleeper = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 30_000)'], {
  stdio: 'ignore',
});
sleeper.unref();
fs.writeFileSync(new URL('./dist/sleeper.pid', import.meta.url), String(sleeper.pid));
