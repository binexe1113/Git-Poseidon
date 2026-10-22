// Real Tauri/WebKit UI, real IPC and Git: worktree List / Inspect / Open (T23-B1).
// The fixture is owned and disposable; read-only flows must leave it byte-identical.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readdirSync,
  readFileSync,
  lstatSync,
  rmSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';
const [binary, output] = process.argv.slice(2);
if (!binary || !output)
  throw Error(
    'Usage: node scripts/worktree-smoke.mjs <runtime-bench binary> <output-dir>',
  );
mkdirSync(output, { recursive: true });
const root = mkdtempSync(join(tmpdir(), 'poseidon-worktrees-'));
const repo = join(root, 'worktree-demo');
const area = join(root, 'area');
mkdirSync(repo);
mkdirSync(area);
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
  assert.equal(r.status, 0, `${args.join(' ')}: ${r.stderr}`);
  return r.stdout.trim();
}
const linked = join(
  area,
  'wt feature 日本 com um nome de diretório bem longo para truncar',
);
git('init', '-q', '--initial-branch=main');
git('config', 'user.name', 'Worktree Demo');
git('config', 'user.email', 'worktree@example.invalid');
git('config', 'commit.gpgsign', 'false');
writeFileSync(join(repo, 'a.txt'), 'base\n');
writeFileSync(join(repo, '.gitignore'), '*.log\n');
git('add', '-A');
git('commit', '-qm', 'Base');
for (const b of ['feature', 'spare', 'other', 'init']) git('branch', b);
git('worktree', 'add', '-q', linked, 'feature');
git('worktree', 'add', '-q', '--detach', join(area, 'wt-detached'), 'HEAD');
git('worktree', 'add', '-q', join(area, 'wt-locked'), 'spare');
git('worktree', 'lock', '--reason', 'disco externo', join(area, 'wt-locked'));
git('worktree', 'add', '-q', join(area, 'wt-gone'), 'other');
rmSync(join(area, 'wt-gone'), { recursive: true });
git('worktree', 'add', '-q', join(area, 'wt-init'), 'init');
// The marker `git worktree add` keeps while running (and leaves after SIGKILL).
writeFileSync(join(repo, '.git/worktrees/wt-init/locked'), 'initializing');
writeFileSync(join(linked, 'staged.txt'), 's\n');
spawnSync('git', ['add', 'staged.txt'], { cwd: linked, env });
writeFileSync(join(linked, 'a.txt'), 'modified\n');
writeFileSync(join(linked, 'untracked.txt'), 'u\n');
writeFileSync(join(linked, 'debug.log'), 'ignored\n');

/** Bytes, modes and mtimes of everything Git or a checkout owns. */
function fingerprint() {
  const out = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name);
      const s = lstatSync(path, { bigint: true });
      let line = `${path.slice(root.length)} ${s.mode} ${s.ino} ${s.size} ${s.mtimeNs} ${s.ctimeNs}`;
      if (s.isFile())
        line +=
          ' ' + createHash('sha256').update(readFileSync(path)).digest('hex');
      out.push(line);
      if (s.isDirectory()) walk(path);
    }
  };
  walk(repo);
  walk(area);
  return out;
}
function same(before, after, label) {
  const changed = after.filter((l, i) => l !== before[i]);
  assert.equal(after.length, before.length, `${label}: entries changed`);
  assert.deepEqual(changed, [], `${label}: repository data changed`);
}

const app = spawn(resolve(binary), [], {
  env: { ...env, WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9243' },
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
      inspector = await connectInspector('http://127.0.0.1:9243');
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
        (await ev(
          `(document.querySelector('.worktree-dialog')??document.querySelector('.worktree-section'))?.textContent`,
        )),
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
  const entries = `[...document.querySelectorAll('.worktree-entry')]`;
  const entry = (fragment) =>
    `${entries}.find(e=>e.title.includes(${JSON.stringify(fragment)}))`;
  async function details(fragment) {
    await ev(`${entry(fragment)}.click();true`);
    await wait(`!!document.querySelector('.worktree-dialog[open]')`);
  }
  async function closeDetails() {
    await click('Fechar', `document.querySelector('.worktree-dialog')`);
    await wait(`!document.querySelector('.worktree-dialog')`);
  }
  const dialogText = `(document.querySelector('.worktree-dialog')?.textContent??'').replace(/\\s+/g,' ')`;

  await resize(1440, 900);
  await wait(`!!document.querySelector('.primary-button:not(:disabled)')`);
  await click('Abrir por caminho…');
  await fill('#repository-path', repo);
  await click('Abrir caminho');
  await wait(`document.querySelectorAll('.commit-row').length>0`);
  await wait(`${entries}.length===6`);
  const start = fingerprint();

  // Listing: kinds, states, current marker.
  assert.ok(
    await ev(`${entry('worktree-demo')}.getAttribute('aria-current')==='true'`),
  );
  for (const [fragment, text] of [
    ['wt-locked', 'bloqueado'],
    ['wt-gone', 'ausente'],
    ['wt-init', 'inicializando'],
    ['wt-detached', 'HEAD destacado'],
    ['wt feature', 'feature'],
  ])
    assert.ok(
      await ev(
        `${entry(fragment)}.textContent.includes(${JSON.stringify(text)})`,
      ),
      `${fragment}: ${text}`,
    );
  checks.list_states = true;
  await shot('01-list');

  // No mutable worktree commands anywhere in the panel.
  const forbidden =
    /criar worktree|remover|mover|podar|prune|reparar|repair|desbloquear|unlock/i;
  const labels = await ev(
    `JSON.stringify([...document.querySelectorAll('.worktree-section button')].map(b=>(b.getAttribute('aria-label')??'')+' '+b.textContent))`,
  );
  assert.ok(!forbidden.test(labels), labels);

  // Keyboard: arrows move along the entries.
  await ev(`${entries}[0].focus();true`);
  await ev(
    `document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));true`,
  );
  assert.ok(await ev(`document.activeElement===${entries}[1]`));
  await ev(
    `document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'End',bubbles:true}));true`,
  );
  assert.ok(await ev(`document.activeElement===${entries}.at(-1)`));
  checks.keyboard = true;

  // Inspect: protected counts and permission wording for the linked worktree.
  await details('wt feature');
  await wait(`(${dialogText}).includes('preparadas')`);
  assert.ok(
    await ev(
      `(${dialogText}).includes('1 preparadas · 1 modificadas · 1 não rastreadas')`,
    ),
  );
  assert.ok(await ev(`(${dialogText}).includes('Confiança do repositório')`));
  const dialogButtons = await ev(
    `JSON.stringify([...document.querySelectorAll('.worktree-dialog button')].map(b=>b.textContent.trim()))`,
  );
  assert.deepEqual(JSON.parse(dialogButtons), ['Fechar', 'Abrir worktree']);
  checks.inspect_linked = true;
  await shot('02-details-linked');
  await closeDetails();
  await details('wt-locked');
  assert.ok(await ev(`(${dialogText}).includes('disco externo')`));
  await shot('03-details-locked');
  await closeDetails();
  await details('wt-gone');
  assert.ok(
    await ev(
      `document.querySelector('.worktree-open').disabled && (${dialogText}).includes('podado')`,
    ),
  );
  await shot('04-details-prunable');
  await closeDetails();
  await details('wt-init');
  assert.ok(await ev(`document.querySelector('.worktree-open').disabled`));
  await closeDetails();
  checks.unavailable_not_openable = true;
  same(start, fingerprint(), 'list/inspect');
  checks.list_inspect_read_only = true;

  // Stale selection: the registry changes behind the open dialog, and the
  // backend refuses the open itself (not only the status read).
  await details('wt feature');
  await wait(`(${dialogText}).includes('preparadas')`);
  git('worktree', 'add', '-q', '--detach', join(area, 'wt-late'), 'HEAD');
  await click('Abrir worktree', `document.querySelector('.worktree-dialog')`);
  await wait(
    `(${dialogText}).includes('A lista de worktrees mudou desde a leitura')`,
  );
  // After the listing refreshes, the dialog stays bound to its old revision.
  await wait(
    `${entries}.length===7 && document.querySelector('.worktree-open').disabled && (${dialogText}).includes('A lista de worktrees mudou desde a leitura')`,
  );
  assert.ok(
    await ev(
      `!!document.querySelector('.repository-card')?.textContent.includes('worktree-demo')`,
    ),
  );
  await shot('05-stale-selection');
  await pause(500);
  assert.ok(
    await ev(
      `${entry('worktree-demo')}?.getAttribute('aria-current')==='true'`,
    ),
    'a refused selection must never open a worktree',
  );
  await closeDetails();
  checks.stale_refused = true;

  // Open linked, then main again: navigation only.
  const beforeOpen = fingerprint();
  await details('wt feature');
  await click('Abrir worktree', `document.querySelector('.worktree-dialog')`);
  await wait(
    `document.querySelector('.head-chip')?.textContent.includes('feature') && ${entries}.length===7 && ${entry('wt feature')}?.getAttribute('aria-current')==='true'`,
  );
  await shot('06-opened-linked');
  checks.open_linked = true;
  await details('worktree-demo');
  await click('Abrir worktree', `document.querySelector('.worktree-dialog')`);
  await wait(
    `document.querySelector('.head-chip')?.textContent.includes('main') && ${entry('worktree-demo')}?.getAttribute('aria-current')==='true'`,
  );
  await shot('07-back-to-main');
  same(beforeOpen, fingerprint(), 'open linked/main');
  assert.equal(git('symbolic-ref', '--short', 'HEAD'), 'main');
  assert.equal(
    spawnSync('git', ['symbolic-ref', '--short', 'HEAD'], {
      cwd: linked,
      env,
      encoding: 'utf8',
    }).stdout.trim(),
    'feature',
  );
  checks.open_round_trip_read_only = true;

  // The open worktree is removed outside the app.
  await details('wt-late');
  await click('Abrir worktree', `document.querySelector('.worktree-dialog')`);
  await wait(`${entry('wt-late')}?.getAttribute('aria-current')==='true'`);
  git('worktree', 'remove', join(area, 'wt-late'));
  await ev(
    `document.querySelector('[aria-label="Atualizar worktrees"]').click();true`,
  );
  await wait(`!!document.querySelector('.worktree-section [role="alert"]')`);
  assert.ok(
    await ev(
      `(()=>{const a=document.querySelector('.worktree-section [role="alert"]').getBoundingClientRect();const s=document.querySelector('.worktree-section').getBoundingClientRect();return a.height>0&&a.top>=s.top&&a.bottom<=s.bottom&&a.bottom<=innerHeight;})()`,
    ),
    'the listing error must be visible, not only present in the DOM',
  );
  assert.ok(
    await ev(
      `document.querySelector('.worktree-section [role="alert"]').textContent.includes('não existe mais') && document.querySelectorAll('.worktree-entry').length===0`,
    ),
    'the listing error names the vanished folder and offers no stale entries',
  );
  await shot('08-removed-while-open');
  checks.removed_while_open_reported = true;
  await click('Abrir por caminho…');
  await fill('#repository-path', repo);
  await click('Abrir caminho');
  await wait(
    `${entries}.length===6 && ${entry('worktree-demo')}?.getAttribute('aria-current')==='true'`,
  );
  checks.recovered_by_opening_main = true;

  for (const [w, h] of [
    [1280, 720],
    [1366, 768],
    [1920, 1080],
    [1000, 900],
  ]) {
    await resize(w, h);
    await shot(`worktrees-${w}x${h}`);
  }
  await details('wt feature');
  await wait(`(${dialogText}).includes('preparadas')`);
  assert.ok(
    await ev(
      `(()=>{const r=document.querySelector('.worktree-dialog').getBoundingClientRect();return r.right<=innerWidth&&r.bottom<=innerHeight&&r.left>=0;})()`,
    ),
  );
  await shot('09-details-1000x900');
  await closeDetails();
  checks.responsive = true;
  checks.pass = true;
  writeFileSync(
    join(output, 'worktree-smoke.json'),
    JSON.stringify(
      {
        checks,
        fixture: root,
        method:
          'runtime-bench Tauri, real IPC and Git, path dialog via UI. Hidden Hyprland workspace, WebKit snapshots. Fingerprints (bytes, modes, inodes, mtimes, ctimes of repository and worktree trees) before/after each read-only flow; external changes happen only outside those windows.',
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
