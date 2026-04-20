import { execSync } from 'node:child_process';
import { cpSync, mkdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');

function run(cmd, cwd) {
  console.log(`\n==> ${cmd} (in ${cwd})`);
  execSync(cmd, { cwd, stdio: 'inherit' });
}

function copy(src, dest) {
  console.log(`\n==> copy ${src} -> ${dest}`);
  mkdirSync(dest, { recursive: true });
  cpSync(src, dest, { recursive: true });
}

// Build Astro site
run('npm install', __dirname);
run('npm run build', __dirname);

// TypeScript docs → dist/dev/hashiverse-client-web/
run('npm install', join(root, 'hashiverse-client-web'));
run('npm run docs', join(root, 'hashiverse-client-web'));
copy(
  join(root, 'hashiverse-client-web/docs'),
  join(__dirname, 'dist/dev/hashiverse-client-web'),
);

// Rust docs → dist/dev/hashiverse-rust/
run('cargo doc --no-deps --workspace --release', join(root, 'hashiverse-rust'));
copy(
  join(root, 'hashiverse-rust/target/doc'),
  join(__dirname, 'dist/dev/hashiverse-rust'),
);

console.log('\n==> Done. Output: www/dist/');
