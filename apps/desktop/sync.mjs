// Copies the ferroterm web component (built WASM + JS sources + CSS) into the
// Tauri front-end dir so it is bundled with the app. Run `../../build.sh` first
// to (re)generate web/pkg, then this to vendor it.

import { cp, rm, access } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const web = join(here, '..', '..', 'web');
const dest = join(here, 'src', 'ferroterm');

try {
  await access(join(web, 'pkg', 'ferroterm_wasm_bg.wasm'));
} catch {
  console.error('web/pkg not found — run ./build.sh at the repo root first.');
  process.exit(1);
}

await rm(dest, { recursive: true, force: true });
await cp(join(web, 'src'), join(dest, 'src'), { recursive: true });
await cp(join(web, 'pkg'), join(dest, 'pkg'), { recursive: true });
await cp(join(web, 'ferroterm.css'), join(dest, 'ferroterm.css'));
console.log('synced ferroterm ->', dest);
