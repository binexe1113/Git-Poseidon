// Distribution contracts: version mappings, embedded assets and desktop identity.
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
const root = resolve(import.meta.dirname, '..');
const read = (name) => readFileSync(resolve(root, name), 'utf8');
const pkg = JSON.parse(read('package.json'));
const lock = JSON.parse(read('package-lock.json'));
const config = JSON.parse(read('src-tauri/tauri.conf.json'));
const version = '0.2.0-alpha';
assert.equal(pkg.version, version);
assert.equal(lock.version, version);
assert.equal(lock.packages[''].version, version);
assert.equal(config.version, version);
const metadata = JSON.parse(
  execFileSync(
    'cargo',
    ['metadata', '--locked', '--offline', '--no-deps', '--format-version', '1'],
    { cwd: root, encoding: 'utf8' },
  ),
);
for (const pkg of metadata.packages)
  assert.equal(pkg.version, version, pkg.name);
assert.equal(config.productName, 'Git-Poseidon');
assert.equal(config.identifier, 'io.gitposeidon.desktop');
assert.equal(config.build.frontendDist, '../dist/frontend');
assert.equal(config.bundle.active, true);
assert.ok(config.bundle.targets.includes('appimage'));
assert.ok(read('vite.config.ts').includes("outDir: 'dist/frontend'"));
for (const icon of config.bundle.icon)
  assert.ok(existsSync(resolve(root, 'src-tauri', icon)), icon);
assert.equal(read('public/poseidon.svg'), read('src-tauri/icons/poseidon.svg'));
assert.ok(
  read('scripts/prepare-release.mjs').includes("    'public',"),
  'Source snapshot must include public branding assets',
);
const desktop = read('packaging/linux/io.gitposeidon.desktop.desktop');
for (const field of [
  'Name=Git-Poseidon',
  'Exec=git-poseidon',
  'TryExec=git-poseidon',
  'Icon=io.gitposeidon.desktop',
  'Terminal=false',
])
  assert.ok(desktop.includes(field), field);
execFileSync('desktop-file-validate', [
  resolve(root, 'packaging/linux/io.gitposeidon.desktop.desktop'),
]);
const arch = read('packaging/arch/PKGBUILD.in');
assert.ok(arch.includes('pkgver=0.2.0alpha'));
assert.ok(arch.includes("license=('MIT')"));
assert.ok(
  arch.includes("'diffutils'"),
  'Commit guard requires /usr/bin/cmp from diffutils',
);
assert.ok(!arch.includes('runtime-bench'));
assert.ok(!pkg.scripts['release:arch'].includes('sudo'));
console.log(
  'Release contracts passed: versions, Arch mapping, assets, icons, desktop entry and production features.',
);
