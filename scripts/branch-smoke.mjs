// Real Tauri/WebKit UI, isolated Git fixture, real IPC. No mocked Git or folder picker.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  unlinkSync,
} from 'node:fs';
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
    'Usage: node scripts/branch-smoke.mjs <runtime-bench binary> <output-dir>',
  );
mkdirSync(output, { recursive: true });
const root = mkdtempSync(join(tmpdir(), 'poseidon-branches-'));
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
    await click('Fechar', `document.querySelector('.branch-dialog')`);
    await wait(`!document.querySelector('.branch-dialog')`);
  }
  async function review() {
    await click('Revisar operação', `document.querySelector('.branch-dialog')`);
    await wait(
      `!document.querySelector('.branch-dialog [role="status"]')?.textContent.includes('Verificando') && !!document.querySelector('.branch-dialog .index-dialog-actions button:not(:disabled)')`,
    );
    await pause(150);
  }
  async function menu(name) {
    await ev(
      `document.querySelector(${JSON.stringify(`[aria-label="Branch ${name}${git('symbolic-ref', '--short', 'HEAD') === name ? ', atual' : ''}"]`)}).click();true`,
    );
    await wait(`!!document.querySelector('.branch-menu[open]')`);
  }
  async function choose(name, label) {
    await menu(name);
    await click(label, `document.querySelector('.branch-menu')`);
    await wait(`!!document.querySelector('.branch-dialog[open]')`);
  }
  async function confirm(label) {
    const t = Date.now();
    await click(label, `document.querySelector('.branch-dialog')`);
    await wait(
      `!![...document.querySelectorAll('.branch-dialog .index-dialog-actions button')].find(b=>b.textContent.trim()==='Fechar'&&!b.disabled)`,
    );
    (timings[label] ??= []).push(Date.now() - t);
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
  await ev(
    `document.querySelector('[aria-label="Criar branch"]').click();true`,
  );
  await fill('#branch-name', 'demo');
  await review();
  await click(
    'Confiar neste repositório',
    `document.querySelector('.branch-dialog')`,
  );
  await wait(
    `!![...document.querySelectorAll('.branch-dialog button')].find(b=>b.textContent.trim()==='Confirmar criação')`,
  );
  await shot('01-create-confirmation');
  await confirm('Confirmar criação');
  assert.equal(git('rev-parse', 'demo'), main);
  checks.create = true;
  await close();
  await menu('demo');
  await ev(
    `document.querySelector('.branch-menu').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));true`,
  );
  assert.ok(
    await ev(`document.activeElement?.getAttribute('role')==='menuitem'`),
  );
  checks.menu_keyboard = true;
  await shot('02-context-menu');
  await click('Trocar para demo', `document.querySelector('.branch-menu')`);
  await review();
  await shot('03-switch-confirmation');
  await confirm('Confirmar troca');
  assert.equal(git('symbolic-ref', '--short', 'HEAD'), 'demo');
  await wait(
    `document.querySelector('.head-chip')?.textContent.includes('demo')`,
  );
  checks.switch_and_head = true;
  await close();
  await choose('demo', 'Renomear demo…');
  await fill('#branch-name', 'renamed');
  await review();
  await confirm('Confirmar renomeação');
  assert.equal(git('symbolic-ref', '--short', 'HEAD'), 'renamed');
  await wait(
    `document.querySelector('.head-chip')?.textContent.includes('renamed')`,
  );
  checks.rename_current = true;
  await close();
  await choose('main', 'Trocar para main');
  await review();
  await confirm('Confirmar troca');
  await close();
  await choose('renamed', 'Excluir renamed…');
  await review();
  await shot('04-safe-delete');
  await confirm('Confirmar exclusão');
  assert.notEqual(
    spawnSync('git', ['show-ref', '--verify', 'refs/heads/renamed'], {
      cwd: repo,
      env,
    }).status,
    0,
  );
  checks.safe_delete = true;
  await close();
  await choose('feature', 'Excluir feature…');
  await review();
  await wait(
    `document.querySelector('.branch-dialog')?.textContent.includes('divergente')`,
  );
  await shot('05-divergent-blocked');
  assert.equal(git('rev-parse', 'feature'), feature);
  checks.divergent_blocked = true;
  await click('Cancelar', `document.querySelector('.branch-dialog')`);
  // Keyboard context menu on a graph row, then create at that immutable commit.
  await ev(
    `document.querySelector('.commit-row').dispatchEvent(new KeyboardEvent('keydown',{key:'F10',shiftKey:true,bubbles:true,cancelable:true}));true`,
  );
  await wait(`!!document.querySelector('.branch-menu[open]')`);
  await click(
    'Criar branch neste commit…',
    `document.querySelector('.branch-menu')`,
  );
  await fill('#branch-name', 'graph-created');
  await review();
  await confirm('Confirmar criação');
  assert.equal(git('rev-parse', 'graph-created'), main);
  checks.graph_context_create = true;
  await close();
  for (const [w, h] of [
    [1280, 720],
    [1366, 768],
    [1440, 900],
    [1920, 1080],
    [1000, 900],
  ]) {
    await resize(w, h);
    await shot(`branches-${w}x${h}`);
    assert.equal(
      await ev('document.documentElement.scrollWidth>innerWidth'),
      false,
    );
  }
  await resize(1440, 900);
  write('feature-only.txt', 'keep untracked\n');
  await choose('feature', 'Trocar para feature');
  await review();
  await wait(
    `document.querySelector('.branch-dialog')?.textContent.includes('impedem')`,
  );
  await shot('06-untracked-blocked');
  assert.equal(git('symbolic-ref', '--short', 'HEAD'), 'main');
  assert.equal(
    readFileSync(join(repo, 'feature-only.txt'), 'utf8'),
    'keep untracked\n',
  );
  checks.untracked_preserved = true;
  await click('Cancelar', `document.querySelector('.branch-dialog')`);
  unlinkSync(join(repo, 'feature-only.txt'));
  writeFileSync(
    join(repo, '.git/hooks/post-checkout'),
    '#!/bin/sh\necho ran >> hook-marker\nexit 1\n',
    { mode: 0o755 },
  );
  await choose('feature', 'Trocar para feature');
  await review();
  await wait(
    `document.querySelector('.branch-dialog')?.textContent.includes('Autorizar hooks de branches')`,
  );
  await shot('07-hook-consent');
  await click(
    'Autorizar hooks de branches',
    `document.querySelector('.branch-dialog')`,
  );
  await wait(
    `!![...document.querySelectorAll('.branch-dialog button')].find(b=>b.textContent.trim()==='Confirmar troca')`,
  );
  await confirm('Confirmar troca');
  assert.equal(git('symbolic-ref', '--short', 'HEAD'), 'feature');
  await wait(
    `document.querySelector('.head-chip')?.textContent.includes('feature')`,
  );
  assert.ok(
    await ev(
      `document.querySelector('.branch-dialog')?.textContent.includes('não confirmado')`,
    ),
  );
  await shot('08-hook-failure-reconciled');
  await pause(600);
  assert.equal(readFileSync(join(repo, 'hook-marker'), 'utf8'), 'ran\n');
  checks.hook_failure_reconciled_no_retry = true;
  await close();
  const after = memory();
  assert.equal(git('rev-parse', 'main'), main);
  assert.equal(git('rev-parse', 'feature'), feature);
  assert.equal(git('rev-parse', 'graph-created'), main);
  assert.equal(git('rev-parse', 'HEAD'), feature);
  assert.equal(git('rev-parse', base), base);
  checks.git_final_state = true;
  checks.upstream_visible = await ev(
    `!!document.querySelector('.upstream-indicator')`,
  );
  checks.pass = true;
  writeFileSync(
    join(output, 'branch-smoke.json'),
    JSON.stringify(
      {
        checks,
        timings_ui_poll_ms: timings,
        memory: { before, after },
        fixture: root,
        mainCommits: commitCount,
        method:
          'runtime-bench Tauri, real IPC and Git, path dialog via UI. Hidden Hyprland workspace, WebKit snapshots; process-only NVIDIA workaround. No frame metrics. Twenty RSS/PSS samples per state. Fixture intentionally changed only by specified operations.',
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
