// Real Tauri/WebKit UI, isolated Git fixture, real IPC. No mocked Git or folder picker.
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';
const [binary, output, corpus = '12'] = process.argv.slice(2);
const commitCount = Number(corpus);
assert.ok(
  Number.isInteger(commitCount) && commitCount >= 1 && commitCount <= 10000,
);
if (!binary || !output)
  throw Error(
    'Usage: node scripts/stash-smoke.mjs <runtime-bench binary> <output-dir>',
  );
mkdirSync(output, { recursive: true });
const root = mkdtempSync(join(tmpdir(), 'poseidon-stash-'));
const repo = join(root, 'branch-demo');
mkdirSync(repo);
const env = Object.fromEntries(
  Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')),
);
Object.assign(env, {
  HOME: root,
  XDG_CONFIG_HOME: join(root, 'config'),
  XDG_DATA_HOME: join(root, 'data'),
  XDG_CACHE_HOME: join(root, 'cache'),
  __NV_DISABLE_EXPLICIT_SYNC: '1',
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: join(root, 'no-global'),
});
function git(...args) {
  const r = spawnSync('git', args, { cwd: repo, env, encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  return r.stdout.trim();
}
function write(path, text) {
  writeFileSync(join(repo, path), text);
}
git('init', '-q', '--initial-branch=main');
git('config', 'user.name', 'Branch Demo');
git('config', 'user.email', 'branch@example.invalid');
git('config', 'commit.gpgsign', 'false');
write('common.txt', 'base\n');
git('add', '-A');
git('commit', '-qm', 'Base das branches');
const base = git('rev-parse', 'HEAD');
git('switch', '-qc', 'feature');
write('feature-only.txt', 'feature\n');
git('add', '-A');
git('commit', '-qm', 'Feature divergente');
const feature = git('rev-parse', 'HEAD');
git('switch', 'main');
let stream = '';
for (let i = 0; i < commitCount; i++) {
  const message = `Histórico principal ${i}`;
  stream += `commit refs/heads/main\nmark :${i + 1}\ncommitter Branch Demo <branch@example.invalid> 1799999999 +0000\ndata ${Buffer.byteLength(message)}\n${message}\nfrom ${i === 0 ? base : ':' + i}\n\n`;
}
const imported = spawnSync('git', ['fast-import', '--quiet'], {
  cwd: repo,
  env,
  input: stream,
  encoding: 'utf8',
});
assert.equal(imported.status, 0, imported.stderr);
const main = git('rev-parse', 'HEAD');
git('branch', '--set-upstream-to=main', 'feature');
const app = spawn(resolve(binary), [], {
  env: { ...env, WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9242' },
  stdio: 'ignore',
});
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const hypr = (lua) =>
  assert.equal(
    spawnSync('hyprctl', ['dispatch', lua], { encoding: 'utf8' }).stdout.trim(),
    'ok',
  );
let inspector;
const checks = {};
const timings = {};
try {
  for (let i = 0; i < 150; i++) {
    if (
      JSON.parse(
        spawnSync('hyprctl', ['clients', '-j'], { encoding: 'utf8' }).stdout,
      ).some((w) => w.pid === app.pid)
    )
      break;
    await pause(100);
  }
  const window = `pid:${app.pid}`;
  hypr(`hl.dsp.window.move({workspace="special:poseidon",window="${window}"})`);
  hypr(`hl.dsp.window.float({action="enable",window="${window}"})`);
  for (let i = 0; i < 150 && !inspector; i++) {
    try {
      inspector = await connectInspector('http://127.0.0.1:9242');
    } catch {
      await pause(100);
    }
  }
  assert.ok(inspector, 'No WebKit inspector');
  await inspector.send('Runtime.enable');
  const ev = inspector.evaluate;
  async function wait(expr) {
    for (let i = 0; i < 400; i++) {
      if (await ev(expr)) return;
      await pause(25);
    }
    throw Error(
      'UI timeout: ' +
        expr +
        '; ' +
        (await ev(`document.querySelector('dialog')?.textContent`)),
    );
  }
  async function click(text, scope = 'document') {
    await ev(
      `(()=>{const b=[...${scope}.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)});if(!b)throw Error('Missing button '+${JSON.stringify(text)});b.click();return true;})()`,
    );
  }
  async function fill(selector, text) {
    await ev(
      `(()=>{const el=document.querySelector(${JSON.stringify(selector)});el.value=${JSON.stringify(text)};el.dispatchEvent(new Event('input',{bubbles:true}));return true;})()`,
    );
  }
  async function shot(name) {
    await pause(300);
    const s = await inspector.send('Page.snapshotRect', {
      x: 0,
      y: 0,
      width: await ev('innerWidth'),
      height: await ev('innerHeight'),
      coordinateSystem: 'Viewport',
    });
    writeFileSync(
      join(output, name + '.png'),
      Buffer.from(s.dataURL.split(',')[1], 'base64'),
    );
  }
  async function resize(w, h) {
    hypr(`hl.dsp.window.resize({x=${w},y=${h},window="${window}"})`);
    await wait(`innerWidth===${w}&&innerHeight===${h}`);
    await pause(300);
  }
  async function close() {
    await click('Fechar', `document.querySelector('.stash-dialog')`);
    await wait(`!document.querySelector('.stash-dialog')`);
  }
  function memory() {
    const r = spawnSync(
      'python3',
      [
        'scripts/measure-linux-memory.py',
        String(app.pid),
        '--samples',
        '20',
        '--interval',
        '.25',
      ],
      { encoding: 'utf8' },
    );
    assert.equal(r.status, 0, r.stderr);
    return JSON.parse(r.stdout);
  }
  await resize(1440, 900);
  await wait(`!!document.querySelector('.primary-button:not(:disabled)')`);
  await click('Abrir por caminho…');
  await fill('#repository-path', repo);
  await click('Abrir caminho');
  await wait(`document.querySelectorAll('.commit-row').length>0`);

  const before = memory();
  const scope = `document.querySelector('.stash-dialog')`;
  async function reviewStash() {
    await click('Revisar operação', scope);
    await wait(`!!document.querySelector('.stash-dialog ul')`);
    await pause(100);
  }
  async function confirmStash(label) {
    const t = Date.now();
    await click(label, scope);
    await wait(`!!document.querySelector('.stash-result')`);
    timings[label] = Date.now() - t;
    assert.ok(
      (
        await ev(`document.querySelector('.stash-result').textContent`)
      ).includes('concluída'),
    );
  }
  write('common.txt', 'staged\n');
  git('add', 'common.txt');
  write('common.txt', 'working 日本\n');
  write('untracked.txt', 'new\n');
  await ev(`document.querySelector('[aria-label="Criar stash"]').click();true`);
  await fill('#stash-message', 'T21 demonstração');
  await ev(
    `document.querySelector('.stash-dialog input[type="checkbox"]').click();true`,
  );
  await reviewStash();
  await click('Confiar neste repositório', scope);
  await wait(
    `!![...document.querySelectorAll('.stash-dialog button')].find(b=>b.textContent.trim()==='Confirmar criação')`,
  );
  await shot('01-create-confirmation');
  await confirmStash('Confirmar criação');
  assert.equal(readFileSync(join(repo, 'common.txt'), 'utf8'), 'base\n');
  assert.equal(git('show', 'stash:common.txt'), 'working 日本');
  assert.equal(git('show', 'stash^2:common.txt'), 'staged');
  assert.equal(git('show', 'stash^3:untracked.txt'), 'new');
  checks.create = true;
  await close();
  await wait(`document.querySelectorAll('.stash-entry').length===1`);
  const t = Date.now();
  await ev(`document.querySelector('.stash-entry').click();true`);
  await wait(`document.querySelectorAll('.layer-tabs button').length===3`);
  timings.details = Date.now() - t;
  await shot('02-details');
  await ev(`document.querySelector('.stash-dialog .file-item').click();true`);
  await wait(
    `document.querySelector('.stash-diff .diff-line')!==null || document.querySelector('.stash-diff .diff-row')!==null`,
  );
  await shot('03-diff');
  await ev(
    `document.querySelector('.stash-diff [aria-label="Fechar diff"]')?.click();true`,
  );
  // Dispatch the viewer's Escape handler through its own root.
  await ev(
    `document.querySelector('.stash-diff .diff-viewer')?.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));true`,
  );
  await wait(`!document.querySelector('.stash-diff')`);
  for (const label of ['Index · base → staged', 'Untracked']) {
    await click(label, scope);
    await wait(`!!document.querySelector('.stash-dialog .file-item')`);
  }
  checks.layers_and_diff = true;
  await click('Aplicar stash', scope);
  await ev(
    `document.querySelector('.stash-dialog input[type="checkbox"]').click();true`,
  );
  await reviewStash();
  await shot('04-apply-confirmation');
  await confirmStash('Confirmar aplicação');
  assert.equal(
    readFileSync(join(repo, 'common.txt'), 'utf8'),
    'working 日本\n',
  );
  assert.equal(git('show', ':common.txt'), 'staged');
  assert.equal(readFileSync(join(repo, 'untracked.txt'), 'utf8'), 'new\n');
  assert.equal(git('stash', 'list').split('\n').length, 1);
  checks.apply_retains_stash_and_restores_index = true;
  await close();
  await ev(
    `document.querySelector('.stash-entry').dispatchEvent(new KeyboardEvent('keydown',{key:'F10',shiftKey:true,bubbles:true}));true`,
  );
  await wait(`!!document.querySelector('.branch-menu[open]')`);
  assert.ok(
    await ev(`document.activeElement.getAttribute('role')==='menuitem'`),
  );
  await shot('05-keyboard-context');
  await click('Excluir stash', `document.querySelector('.branch-menu')`);
  await reviewStash();
  await shot('06-drop-confirmation');
  await confirmStash('Confirmar exclusão');
  await close();
  await wait(`document.querySelectorAll('.stash-entry').length===0`);
  assert.equal(git('stash', 'list'), '');
  checks.drop = true;
  // External writers are observed without refreshing the entire repository.
  git('stash', 'push', '-u', '-m', 'externo');
  await wait(`document.querySelectorAll('.stash-entry').length===1`);
  checks.external_refresh = true;
  for (const [w, h] of [
    [1280, 720],
    [1366, 768],
    [1440, 900],
    [1920, 1080],
    [1000, 900],
  ]) {
    await resize(w, h);
    assert.ok(await ev(`document.documentElement.scrollWidth<=innerWidth`));
    await shot(`07-layout-${w}x${h}`);
  }
  // Large list remains bounded on initial render, with explicit incremental loading.
  const stored = git('rev-parse', 'stash');
  const reflogPath = join(repo, '.git/logs/refs/stash');
  const originalLog = readFileSync(reflogPath, 'utf8');
  const lastLine = originalLog.trimEnd().split('\n').at(-1);
  const duplicate = stored + lastLine.slice(lastLine.indexOf(' ')) + '\n';
  writeFileSync(reflogPath, originalLog + duplicate.repeat(160));
  await wait(
    `document.querySelector('.stash-section .count')?.textContent==='161'`,
  );
  assert.equal(
    await ev(`document.querySelectorAll('.stash-entry').length`),
    50,
  );
  await click('Mostrar mais', `document.querySelector('.stash-section')`);
  assert.equal(
    await ev(`document.querySelectorAll('.stash-entry').length`),
    100,
  );
  checks.large_list_incremental = true;
  await resize(1440, 900);
  await pause(3000);
  const afterLarge = memory();
  await ev(`document.querySelector('.stash-entry').click();true`);
  await wait(`!!document.querySelector('.layer-tabs')`);
  await click('Excluir stash', scope);
  await reviewStash();
  writeFileSync(reflogPath, readFileSync(reflogPath, 'utf8') + duplicate);
  await wait(
    `document.querySelector('.stash-dialog [role="alert"]')?.textContent.includes('lista mudou')`,
  );
  assert.ok(
    await ev(
      `![...document.querySelectorAll('.stash-dialog button')].some(b=>b.textContent.trim()==='Confirmar exclusão')`,
    ),
  );
  assert.equal(git('stash', 'list').split('\n').length, 162);
  await shot('08-stale-confirmation');
  await click('Cancelar', scope);
  checks.stale_confirmation_invalidated = true;
  for (let i = 0; i < 162; i++) git('stash', 'drop', 'stash@{0}');
  await wait(`document.querySelectorAll('.stash-entry').length===0`);
  await ev(
    `document.querySelector('[aria-label="Atualizar repositório"]').click();true`,
  );
  await wait(`document.querySelector('.stat-button b')?.textContent==='0'`);
  await pause(3000);
  const after = memory();
  write('common.txt', 'stash conflict\n');
  git('stash', 'push', '-m', 'conflito-stash');
  const conflictOid = git('rev-parse', 'stash');
  write('common.txt', 'new base\n');
  git('add', 'common.txt');
  git('commit', '-qm', 'base changed');
  await ev(
    `document.querySelector('[aria-label="Atualizar repositório"]').click();true`,
  );
  await wait(
    `document.querySelector('.commit-row')?.textContent.includes('base changed')`,
  );
  await wait(`document.querySelectorAll('.stash-entry').length===1`);
  await ev(`document.querySelector('.stash-entry').click();true`);
  await wait(`!!document.querySelector('.layer-tabs')`);
  await click('Aplicar stash', scope);
  await reviewStash();
  await click('Confirmar aplicação', scope);
  await wait(
    `document.querySelector('.stash-result')?.textContent.includes('incerto')`,
  );
  await wait(
    `document.querySelector('.stash-observed')?.textContent.includes('1 conflitos')`,
  );
  assert.notEqual(git('ls-files', '-u'), '');
  assert.equal(git('rev-parse', 'stash'), conflictOid);
  assert.ok(readFileSync(join(repo, 'common.txt'), 'utf8').includes('<<<<<<<'));
  await shot('09-conflict-observed');
  await close();
  checks.conflicts_real_state_preserves_stash = true;
  checks.pass = true;
  writeFileSync(
    join(output, 'stash-smoke.json'),
    JSON.stringify(
      {
        checks,
        timings_ui_poll_ms: timings,
        memory: { before, afterLarge, after },
        memoryWindow: { width: 1440, height: 900 },
        fixture: root,
        method:
          'Real Tauri/WebKit, real IPC/Git; native path dialog, WebKit inspector input and screenshots; process-only NVIDIA workaround. RSS/PSS: twenty samples per state.',
      },
      null,
      2,
    ) + '\n',
  );
  console.log(JSON.stringify(checks));
} finally {
  inspector?.close();
  if (app.exitCode === null) {
    app.kill('SIGTERM');
    await Promise.race([new Promise((r) => app.once('exit', r)), pause(3000)]);
    if (app.exitCode === null && !app.signalCode) app.kill('SIGKILL');
  }
}
