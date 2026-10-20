// Build from an explicit local snapshot, including uncommitted implementation.
// No git archive (would omit uncommitted files), privileged commands or publishing.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  readFileSync,
  writeFileSync,
  mkdirSync,
  copyFileSync,
  readdirSync,
  statSync,
  existsSync,
} from 'node:fs';
import { resolve, join } from 'node:path';
import assert from 'node:assert/strict';
const repo = resolve(import.meta.dirname, '..');
const version = JSON.parse(readFileSync(join(repo, 'package.json'))).version;
assert.equal(version, '0.2.0-alpha');
const release = join(repo, 'dist/releases', `v${version}`);
const work = join(release, 'arch-build');
mkdirSync(work, { recursive: true });
function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: repo,
    stdio: 'inherit',
    ...options,
  });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed (${result.status})`);
  return result;
}
const hash = (file) =>
  createHash('sha256').update(readFileSync(file)).digest('hex');
const sourceName = `git-poseidon-${version}-source.tar.gz`;
const source = join(work, sourceName);
// Allowlist excludes .git, local agent settings, credentials, node_modules,
// build outputs, and screenshots. GNU tar normalizes ordering, owner and mtime.
const existingArtifacts = readdirSync(release).some((name) =>
  /\.(pkg\.tar\.zst|AppImage)$/.test(name),
);
if (process.argv.includes('--arch')) {
  assert.ok(
    !readdirSync(work).some((name) => name.endsWith('.pkg.tar.zst')),
    'Package already exists. Use --arch-rebuild explicitly to replace this local build.',
  );
}
if (
  !process.argv.includes('--manifest-only') &&
  (!existingArtifacts ||
    process.argv.includes('--arch') ||
    process.argv.includes('--arch-rebuild'))
) {
  const entries = [
    'Cargo.toml',
    'Cargo.lock',
    'package.json',
    'package-lock.json',
    'index.html',
    'vite.config.ts',
    'svelte.config.js',
    'tsconfig.json',
    '.prettierrc.json',
    '.prettierignore',
    'README.md',
    'public',
    'src',
    'src-tauri',
    'crates',
    'scripts',
    'packaging',
    'CHANGELOG.md',
    'LICENSING.md',
    'LICENSE',
  ];
  const epochResult = spawnSync('git', ['log', '-1', '--format=%ct'], {
    cwd: repo,
    encoding: 'utf8',
  });
  const epoch = process.env.SOURCE_DATE_EPOCH ?? epochResult.stdout.trim();
  assert.match(epoch, /^\d+$/);
  const tar = spawnSync(
    'tar',
    [
      '--sort=name',
      `--mtime=@${epoch}`,
      '--owner=0',
      '--group=0',
      '--numeric-owner',
      '--exclude=src-tauri/target',
      '--exclude=src-tauri/gen',
      '--exclude=__pycache__',
      '--exclude=*.pyc',
      '--exclude=*.log',
      `--transform=s,^,git-poseidon-${version}/,`,
      '-cf',
      '-',
      ...entries,
    ],
    { cwd: repo, maxBuffer: 64 * 1024 * 1024 },
  );
  assert.equal(tar.status, 0, String(tar.stderr));
  const compressed = spawnSync('gzip', ['-n', '-9'], {
    input: tar.stdout,
    maxBuffer: 64 * 1024 * 1024,
  });
  assert.equal(compressed.status, 0, String(compressed.stderr));
  writeFileSync(source, compressed.stdout);
  copyFileSync(source, join(release, sourceName));
  const template = readFileSync(
    join(repo, 'packaging/arch/PKGBUILD.in'),
    'utf8',
  );
  writeFileSync(
    join(work, 'PKGBUILD'),
    template.replace('@SOURCE_SHA256@', hash(source)),
  );
  copyFileSync(join(work, 'PKGBUILD'), join(release, 'PKGBUILD'));
  writeFileSync(
    join(release, 'source-provenance.json'),
    JSON.stringify(
      {
        version,
        sourceDateEpoch: Number(epoch),
        sourceSha256: hash(source),
        includesUncommittedFiles: true,
        entries,
        gitHead: spawnSync('git', ['rev-parse', 'HEAD'], {
          cwd: repo,
          encoding: 'utf8',
        }).stdout.trim(),
        warning:
          'This snapshot is the build input; HEAD alone does not identify this release. Local build snapshot; binary redistribution requires third-party license review.',
      },
      null,
      2,
    ) + '\n',
  );
  if (
    process.argv.includes('--arch') ||
    process.argv.includes('--arch-rebuild')
  ) {
    const rebuild = process.argv.includes('--arch-rebuild');
    if (rebuild) run('bsdtar', ['-xf', source, '-C', join(work, 'src')]);
    run(
      'makepkg',
      rebuild
        ? ['--noextract', '--force', '--noconfirm']
        : ['--cleanbuild', '--noconfirm'],
      {
        cwd: work,
        env: { ...process.env, SOURCE_DATE_EPOCH: epoch },
      },
    );
    for (const name of readdirSync(work).filter((name) =>
      name.endsWith('.pkg.tar.zst'),
    ))
      copyFileSync(join(work, name), join(release, name));
  }
}
const notes = join(repo, 'CHANGELOG.md');
if (existsSync(notes)) copyFileSync(notes, join(release, 'RELEASE-NOTES.md'));
function artifactNames(dir = release, prefix = '') {
  return readdirSync(dir)
    .sort()
    .flatMap((name) => {
      if (
        name === 'arch-build' ||
        ['manifest.json', 'SHA256SUMS'].includes(name)
      )
        return [];
      const relative = prefix + name;
      return statSync(join(dir, name)).isDirectory()
        ? artifactNames(join(dir, name), relative + '/')
        : [relative];
    });
}
const artifacts = artifactNames().map((name) => ({
  name,
  bytes: statSync(join(release, name)).size,
  sha256: hash(join(release, name)),
}));
writeFileSync(
  join(release, 'manifest.json'),
  JSON.stringify(
    { version, publicRelease: false, license: 'MIT', artifacts },
    null,
    2,
  ) + '\n',
);
writeFileSync(
  join(release, 'SHA256SUMS'),
  artifacts.map((a) => `${a.sha256}  ${a.name}\n`).join(''),
);
console.log(`Local release artifacts: ${release}`);
