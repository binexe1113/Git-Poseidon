// Real Tauri commit creation, hooks, drafts and graph refresh (docs/18).
// Disposable fixture only. Usage: node scripts/commit-smoke.mjs <runtime-bench binary> <output-dir>
// Hyprland >= 0.55: the window floats in the hidden `special:poseidon` workspace;
// scroll and focus events are dispatched explicitly (no frame callbacks there).
// Reported write timings come from Rust (Git + reconciliation), excluding paint.
import { spawn, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  readdirSync,
  rmSync,
} from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { connectInspector } from './webkit-inspector.mjs';

const [binary, outDir] = process.argv.slice(2);
if (!binary || !outDir)
  throw new Error('Usage: commit-smoke.mjs <binary> <output-dir>');
mkdirSync(outDir, { recursive: true });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const root = mkdtempSync(join(tmpdir(), 'poseidon-commit-'));
const repo = join(root, 'commit-demo');
mkdirSync(repo);
const env = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
Object.assign(env, {
  HOME: root,
  XDG_CONFIG_HOME: join(root, 'xdg'),
  XDG_DATA_HOME: join(root, 'app-data'),
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: join(root, 'no-global'),
  GIT_AUTHOR_NAME: 'Equipe Demo',
  GIT_AUTHOR_EMAIL: 'demo@example.invalid',
  GIT_COMMITTER_NAME: 'Equipe Demo',
  GIT_COMMITTER_EMAIL: 'demo@example.invalid',
  GIT_AUTHOR_DATE: '2025-03-01T12:00:00+00:00',
  GIT_COMMITTER_DATE: '2025-03-01T12:00:00+00:00',
  TZ: 'UTC',
});
function run(args, ok = [0]) {
  const r = spawnSync('git', args, {
    cwd: repo,
    env,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  assert.ok(ok.includes(r.status), `${args.join(' ')}: ${r.stderr}`);
  return r.stdout;
}
const git = (...args) => run(args);
const write = (path, data) => {
  mkdirSync(join(repo, path, '..'), { recursive: true });
  writeFileSync(join(repo, path), data);
};
function fingerprint(path = repo, files = {}) {
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const next = join(path, entry.name);
    if (entry.isDirectory()) fingerprint(next, files);
    else
      files[relative(repo, next)] = createHash('sha256')
        .update(readFileSync(next))
        .digest('hex');
  }
  return files;
}

// --- Disposable fixture ----------------------------------------------------
git('init', '-q', '--initial-branch=main');
git('config', 'user.name', 'Commit Demo');
git('config', 'user.email', 'commit@example.invalid');
git('config', 'commit.gpgsign', 'false');
write('src/app.rs', 'fn main() { println!("base"); }\n');
write('notes.txt', 'untouched\n');
git('add', '-A');
git('commit', '-q', '-m', 'Base do MVP');
const baseHead = git('rev-parse', 'HEAD').trim();
write('src/app.rs', 'fn main() { println!("staged version"); }\n');
write('notes.txt', 'must remain unstaged\n');
// --- App -------------------------------------------------------------------
const endpoint = 'http://127.0.0.1:9239';
const app = spawn(binary, [], {
  env: { ...env, WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9239' },
  stdio: 'ignore',
});
let inspector;
const hypr = (lua) =>
  assert.equal(
    spawnSync('hyprctl', ['dispatch', lua], { encoding: 'utf8' }).stdout.trim(),
    'ok',
  );
const stats = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    samples: sorted.length,
    p50: sorted[Math.ceil(sorted.length * 0.5) - 1],
    p95: sorted[Math.ceil(sorted.length * 0.95) - 1],
    max: sorted.at(-1),
  };
};
function memory() {
  const result = spawnSync(
    'python3',
    [
      'scripts/measure-linux-memory.py',
      String(app.pid),
      '--samples',
      '5',
      '--interval',
      '0.1',
    ],
    { encoding: 'utf8' },
  );
  assert.equal(result.status, 0, result.stderr);
  const v = JSON.parse(result.stdout);
  return { rss_mib: v.rss_mib, pss_mib: v.pss_mib };
}
const report = { checks: {}, latency_ms: {}, dom: {} };
try {
  for (let i = 0; i < 100; i++) {
    const clients = JSON.parse(
      spawnSync('hyprctl', ['clients', '-j'], { encoding: 'utf8' }).stdout,
    );
    if (clients.some((c) => c.pid === app.pid)) break;
    await pause(100);
  }
  const window = `pid:${app.pid}`;
  hypr(
    `hl.dsp.window.move({ workspace = "special:poseidon", window = "${window}" })`,
  );
  hypr(`hl.dsp.window.float({ action = "enable", window = "${window}" })`);
  for (let i = 0; i < 100 && !inspector; i++) {
    try {
      inspector = await connectInspector(endpoint);
    } catch {
      await pause(100);
    }
  }
  await inspector.send('Runtime.enable');
  const ev = inspector.evaluate;
  const waitFor = async (expression, label = expression) => {
    for (let i = 0; i < 300; i++) {
      if (await ev(expression)) return;
      await pause(25);
    }
    throw new Error(
      'UI condition failed: ' +
        label +
        '; ' +
        JSON.stringify(
          await ev(
            `({ text:document.querySelector('#inspector-changes')?.textContent, ctx:window.__ctx,last:window.__last, descriptor:Object.getOwnPropertyDescriptor(window.__TAURI_INTERNALS__,'invoke'),requests:window.__requests })`,
          ),
        ),
    );
  };
  // WebKit's Runtime.evaluate does not await promises: poll a settled global.
  const evAsync = async (expression) => {
    await ev(
      `window.__r = undefined; Promise.resolve().then(() => ${expression}).then((v) => { window.__r = { v }; }, (e) => { window.__r = { e: String(e) }; }); true`,
    );
    await waitFor('window.__r !== undefined', expression.slice(0, 80));
    const result = await ev('window.__r');
    if ('e' in result) throw new Error(result.e);
    return result.v;
  };
  const resize = async (w, h) => {
    hypr(`hl.dsp.window.resize({ x = ${w}, y = ${h}, window = "${window}" })`);
    await waitFor(`innerWidth === ${w} && innerHeight === ${h}`);
    await pause(250);
  };
  const shot = async (name) => {
    await pause(350);
    const s = await inspector.send('Page.snapshotRect', {
      x: 0,
      y: 0,
      width: await ev('innerWidth'),
      height: await ev('innerHeight'),
      coordinateSystem: 'Viewport',
    });
    writeFileSync(
      join(outDir, `${name}.png`),
      Buffer.from(s.dataURL.split(',')[1], 'base64'),
    );
  };
  await resize(1440, 900);
  await waitFor(
    'document.querySelector(".primary-button") && !document.querySelector(".primary-button").disabled',
  );
  await ev(`window.__ctx=null; window.__writes=[]; window.__last=null;
    window.__ipc=window.__TAURI_INTERNALS__.invoke.bind(window.__TAURI_INTERNALS__);
    window.__item=(area,path)=>[...(document.querySelector('#group-'+area)?.closest('section').querySelectorAll('.file-item')??[])].find(b=>b.title.split(' · ')[0]===path);
    window.__original=window.fetch;
    window.fetch=(u,o)=>decodeURIComponent(String(u)).includes('plugin:dialog|open')?Promise.resolve(new Response(${JSON.stringify(JSON.stringify(repo))},{headers:{'Tauri-Response':'ok','Content-Type':'application/json'}})):window.__original(u,o).then(async r=>{
      const cmd=decodeURIComponent(String(u)).split('/').at(-1);
      if(r.headers.get('Tauri-Response')==='ok' && ['get_working_changes','create_commit'].includes(cmd)) {
        const v=await r.clone().json();
        if(cmd==='get_working_changes'){ window.__ctx=JSON.parse(o.body);window.__last=v.data; }
        else window.__writes.push(v.data);
      }
      return r;
    });
    document.querySelector('.primary-button').click(); true`);
  await waitFor('document.querySelectorAll(".commit-row").length===1');
  await ev(`document.querySelector('#tab-changes').click();true`);
  await waitFor(
    `!!window.__item('unstaged','src/app.rs') && !document.querySelector('.commit-composer').getAttribute('aria-busy')?.includes('true')`,
  );
  const open = async (area, path) => {
    await ev(
      `window.__item(${JSON.stringify(area)},${JSON.stringify(path)}).click();true`,
    );
    await waitFor('!!document.querySelector(".diff-row")');
  };
  const action = (area, path) =>
    ev(
      `window.__item(${JSON.stringify(area)},${JSON.stringify(path)}).closest('.file-row').querySelector('.row-action').click();true`,
    );
  const edit = async (selector, value) =>
    ev(
      `(()=>{ const el=document.querySelector(${JSON.stringify(selector)});el.value=${JSON.stringify(value)};el.dispatchEvent(new Event('input',{bubbles:true}));return true;})()`,
    );
  const settled = () =>
    waitFor(
      `!!window.__last && document.querySelector('.commit-composer')?.getAttribute('aria-busy')==='false' && !document.querySelector('.row-action:disabled')`,
    );
  await open('unstaged', 'src/app.rs');
  await shot('01-modified-diff');
  await action('unstaged', 'src/app.rs');
  await waitFor('!!document.querySelector(".index-dialog[open]")');
  await ev(
    `document.querySelector('.index-dialog .primary-button').click();true`,
  );
  await settled();
  await action('unstaged', 'src/app.rs');
  await waitFor(
    `!!window.__item('staged','src/app.rs') && !window.__item('unstaged','src/app.rs')`,
  );
  // Leave a second version unstaged, after stage and before composing.
  write('src/app.rs', 'fn main() { println!("still editing"); }\n');
  await ev('window.dispatchEvent(new Event("focus"));true');
  await settled();
  await open('staged', 'src/app.rs');
  await edit('#commit-subject', 'Concluir fluxo local do MVP');
  await ev(`document.querySelector('.compose-description').click();true`);
  await edit(
    '#commit-body',
    'Stage explícito, sem incluir edições pendentes.\n\nMensagem multilinha pelo IPC.',
  );
  for (const [w, h] of [
    [1280, 720],
    [1366, 768],
    [1440, 900],
    [1920, 1080],
    [1000, 900],
  ]) {
    await resize(w, h);
    const geometry = await ev(
      `({overflow:document.documentElement.scrollWidth>innerWidth,graphWidth:document.querySelector('.center-pane').getBoundingClientRect().width,composerVisible:document.querySelector('.commit-composer').getBoundingClientRect().height>0})`,
    );
    assert.equal(geometry.overflow, false);
    assert.ok(geometry.composerVisible);
    report.dom[`${w}x${h}`] = geometry;
    await shot(`02-compose-${w}x${h}`);
  }
  await resize(1440, 900);
  await waitFor(`!document.querySelector('.compose-submit').disabled`);
  await ev(`document.querySelector('.compose-submit').click();true`);
  await waitFor('!!document.querySelector(".commit-dialog[open]")');
  assert.equal(
    await ev(`document.activeElement.textContent.trim()`),
    'Cancelar',
  );
  await shot('03-confirmation');
  await ev(
    `document.querySelector('.commit-dialog .toolbar-button').click();true`,
  );
  await settled();
  assert.equal(git('rev-parse', 'HEAD').trim(), baseHead);
  assert.equal(
    await ev('document.querySelector("#commit-subject").value'),
    'Concluir fluxo local do MVP',
  );
  report.checks.cancel_keeps_draft = true;
  report.memory_before = memory();
  await ev(`document.querySelector('.compose-submit').click();true`);
  await waitFor('!!document.querySelector(".commit-dialog[open]")');
  await ev(
    `document.querySelector('.commit-confirm').click(); document.querySelector('.commit-confirm')?.click();true`,
  );
  await waitFor(
    `window.__writes.length===1 && document.querySelectorAll('.commit-row').length===2 && document.querySelector('.commit-row.selected')?.textContent.includes('Concluir fluxo local do MVP')`,
  );
  await settled();
  const newHead = git('rev-parse', 'HEAD').trim();
  assert.notEqual(newHead, baseHead);
  assert.equal(
    git('show', 'HEAD:src/app.rs'),
    'fn main() { println!("staged version"); }\n',
  );
  assert.equal(git('show', 'HEAD:notes.txt'), 'untouched\n');
  assert.equal(
    readFileSync(join(repo, 'notes.txt'), 'utf8'),
    'must remain unstaged\n',
  );
  assert.equal(
    readFileSync(join(repo, 'src/app.rs'), 'utf8'),
    'fn main() { println!("still editing"); }\n',
  );
  assert.ok(
    git('log', '-1', '--format=%B').includes('Mensagem multilinha pelo IPC.'),
  );
  assert.equal(await ev('document.querySelector("#commit-subject").value'), '');
  assert.equal(git('rev-list', '--count', 'HEAD').trim(), '2');
  report.memory_after = memory();
  await shot('04-created-and-graph');
  report.checks.full_mvp_index_only_graph_refresh = true;
  report.checks.double_submission_only_one_commit = true;

  // Authorize hooks through the real UI; rejecting hook preserves the draft.
  git('add', 'notes.txt');
  mkdirSync(join(repo, 'custom-hooks'));
  git('config', 'core.hooksPath', 'custom-hooks');
  writeFileSync(
    join(repo, 'custom-hooks/pre-commit'),
    '#!/bin/sh\nsleep 0.35\necho ran > hook-ran\nexit 1\n',
    { mode: 0o755 },
  );
  await ev('window.dispatchEvent(new Event("focus"));true');
  await settled();
  await edit('#commit-subject', 'Rascunho preservado em erro');
  await waitFor(
    `document.querySelector('.compose-hooks')?.textContent.includes('sem autorização')`,
  );
  await ev(
    `document.querySelector('.compose-hooks .text-button').click();true`,
  );
  await waitFor(
    `document.querySelector('.commit-dialog h2')?.textContent.includes('Autorizar hooks')`,
  );
  await shot('05-hooks-consent');
  await ev(`document.querySelector('.commit-confirm').click();true`);
  await settled();
  await waitFor(`!document.querySelector('.compose-submit').disabled`);
  await ev(`document.querySelector('.compose-submit').click();true`);
  await waitFor('!!document.querySelector(".commit-dialog[open]")');
  await ev(
    `window.__heartbeat=[];window.__timer=setInterval(()=>window.__heartbeat.push(performance.now()),10); document.querySelector('.commit-confirm').click();true`,
  );
  await waitFor(
    `window.__writes.length===2 && window.__writes[1].outcome==='rejected'`,
  );
  await settled();
  assert.equal(git('rev-parse', 'HEAD').trim(), newHead);
  assert.equal(readFileSync(join(repo, 'hook-ran'), 'utf8'), 'ran\n');
  assert.equal(
    await ev('document.querySelector("#commit-subject").value'),
    'Rascunho preservado em erro',
  );
  await shot('06-hook-rejection-draft');
  const heartbeat = await ev(
    `clearInterval(window.__timer);window.__heartbeat`,
  );
  assert.ok(heartbeat.length >= 10, 'WebView timer stopped during slow hook');
  report.responsiveness = {
    timer_samples: heartbeat.length,
    max_interval_ms: Math.max(
      ...heartbeat.slice(1).map((t, i) => t - heartbeat[i]),
    ),
    note: '10ms timers in hidden workspace during 350ms rejecting hook; not compositor frame times',
  };
  report.checks.explicit_hooks_and_rejection_preserve_draft = true;
  // Revoke via UI and assert direct IPC cannot bypass it.
  await ev(
    `document.querySelector('.compose-hooks .text-button').click();true`,
  );
  await settled();
  await waitFor(
    `document.querySelector('.compose-hooks')?.textContent.includes('sem autorização')`,
  );
  const denied = await evAsync(
    `window.__ipc('prepare_commit',{...window.__ctx,revision:window.__last.revision}).then(p=>window.__ipc('create_commit',{...window.__ctx,token:p.data.token,message:'unauthorized'})).then(()=>null,e=>e.code)`,
  );
  assert.equal(denied, 'TrustRequired');
  report.checks.hook_revocation_backend = true;
  git('config', 'commit.gpgsign', 'true');
  git('config', 'gpg.format', 'ssh');
  await ev('window.dispatchEvent(new Event("focus"));true');
  await settled();
  await waitFor(
    `document.querySelector('.compose-block')?.textContent.includes('Assinatura')`,
  );
  assert.ok(await ev('document.querySelector(".compose-submit").disabled'));
  await shot('07-signing-blocked');
  report.checks.signing_explicitly_blocked = true;
  // A future-dated branch pushes HEAD beyond page zero; creation must reveal its real node.
  git('config', 'commit.gpgsign', 'false');
  git('config', 'core.hooksPath', '/dev/null');
  const tree = git('rev-parse', 'HEAD^{tree}').trim();
  let parent = git('rev-parse', 'HEAD').trim();
  for (let i = 0; i < 220; i++) {
    const r = spawnSync(
      'git',
      ['commit-tree', tree, '-p', parent, '-m', `Future ${i}`],
      {
        cwd: repo,
        env: {
          ...env,
          GIT_AUTHOR_DATE: '2030-01-01T00:00:00Z',
          GIT_COMMITTER_DATE: '2030-01-01T00:00:00Z',
        },
        encoding: 'utf8',
      },
    );
    assert.equal(r.status, 0, r.stderr);
    parent = r.stdout.trim();
  }
  git('update-ref', 'refs/heads/future', parent);
  await ev('window.dispatchEvent(new Event("focus"));true');
  await settled();
  await edit('#commit-subject', 'Novo commit além da primeira página');
  await waitFor(`!document.querySelector('.compose-submit').disabled`);
  await ev(`document.querySelector('.compose-submit').click();true`);
  await waitFor('!!document.querySelector(".commit-dialog[open]")');
  await ev(
    `window.__jumpStart=performance.now();document.querySelector('.commit-confirm').click();true`,
  );
  await waitFor(
    `window.__writes.length===3 && document.querySelector('.commit-row.selected')?.textContent.includes('Novo commit além da primeira página')`,
  );
  report.history_jump_dom_observed_ms = await ev(
    'performance.now()-window.__jumpStart',
  );
  assert.equal(
    await ev('document.querySelector(".commit-row.selected").id'),
    'commit-' + git('rev-parse', 'HEAD').trim(),
  );
  await settled();
  await shot('08-created-beyond-first-page');
  report.checks.created_oid_revealed_beyond_page_zero = true;
  report.latency_ms.writes = await ev(
    'window.__writes.map(({outcome,gitMs,hooksMs,refreshMs,invalidationUs,elapsedMs})=>({outcome,gitMs,hooksMs,refreshMs,invalidationUs,elapsedMs}))',
  );
  report.pass = true;
  writeFileSync(
    join(outDir, 'commit-smoke.json'),
    JSON.stringify(report, null, 2) + '\n',
  );
  console.log(JSON.stringify(report, null, 2));
} finally {
  inspector?.close();
  app.kill('SIGTERM');
  await pause(500);
  if (app.exitCode === null && app.signalCode === null) app.kill('SIGKILL');
  rmSync(root, { recursive: true, force: true });
}
