const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');

// Exercise the actual App effect across the asynchronous activation transition.
const source = fs.readFileSync(path.join(__dirname, '../src/App.tsx'), 'utf8');
const ast = ts.createSourceFile('App.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let updaterEffect;
let installHandler;
function visit(node) {
  if (ts.isCallExpression(node) && node.expression.getText(ast) === 'useEffect'
      && node.arguments[0]?.getText(ast).includes('await checkForUpdate(')) {
    updaterEffect = node.getText(ast);
  }
  if (ts.isVariableDeclaration(node) && node.name.getText(ast) === 'installAvailableUpdate') {
    installHandler = node.initializer.getText(ast);
  }
  ts.forEachChild(node, visit);
}
visit(ast);
assert.ok(updaterEffect, 'App update effect exists');
const effectJs = ts.transpileModule(updaterEffect, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
const tick = () => new Promise(resolve => setImmediate(resolve));

function harness(check = async () => null) {
  let previous, cleanup, checks = 0, available = null;
  const warnings = [];
  const context = {
    console: { warn: (...args) => warnings.push(args) },
    IS_SECTOR_VIEW_WINDOW: false, donationActivationChecked: false, donationActivated: false,
    updateCheckStarted: { current: false },
    checkForUpdate: async () => { checks++; return check(); },
    setAvailableUpdate: value => { available = value; },
    useEffect(fn, deps) {
      if (!previous || deps.some((value, i) => !Object.is(value, previous[i]))) {
        cleanup?.(); previous = deps; cleanup = fn();
      }
    },
  };
  vm.createContext(context);
  return { context, warnings, checks: () => checks, available: () => available,
    render: () => vm.runInContext(effectJs, context),
    dispose: () => cleanup?.(),
    replay: () => { cleanup?.(); previous = undefined; vm.runInContext(effectJs, context); },
  };
}

test('saved activation after Strict Mode replay discovers the update', async () => {
  const update = { version: '2.0.15' };
  const h = harness(async () => update);
  h.render(); h.replay();
  h.context.donationActivated = h.context.donationActivationChecked = true;
  h.render(); await tick();
  assert.equal(h.checks(), 1);
  assert.equal(h.available(), update);
});

test('sector windows, offline failure and no newer release keep the tile unchanged', async () => {
  for (const mode of ['sector', 'offline', 'current']) {
    const h = harness(async () => { if (mode === 'offline') throw Error('offline'); return null; });
    h.context.IS_SECTOR_VIEW_WINDOW = mode === 'sector';
    h.context.donationActivated = h.context.donationActivationChecked = true;
    h.render(); await tick(); h.render();
    assert.equal(h.checks(), mode === 'sector' ? 0 : 1);
    assert.equal(h.available(), null);
    assert.equal(h.warnings.length, mode === 'offline' ? 1 : 0);
  }
});

test('closing App during a pending check disposes its returned updater resource', async () => {
  let resolve, closed = 0;
  const h = harness(() => new Promise(r => { resolve = r; }));
  h.context.donationActivated = h.context.donationActivationChecked = true;
  h.render(); h.dispose();
  resolve({ close: async () => { closed++; } });
  await tick();
  assert.equal(h.available(), null);
  assert.equal(closed, 1);
});

test('installation blocks double clicks, restarts after success and permits retry after failure', async () => {
  assert.ok(installHandler);
  const handlerJs = ts.transpileModule(installHandler, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  let installs = 0, resolve, reject, installing = false;
  const commands = [], warnings = [];
  const context = {
    console: { warn: (...args) => warnings.push(args) },
    availableUpdate: { downloadAndInstall: () => { installs++; return new Promise((a, b) => { resolve = a; reject = b; }); } },
    updateInstallStarted: { current: false }, installingUpdate: false,
    setInstallingUpdate: value => { installing = value; },
    invoke: async command => { commands.push(command); },
    useCallback: fn => fn,
  };
  const install = vm.runInNewContext(handlerJs, context);
  const first = install(); const second = install();
  assert.equal(installs, 1);
  assert.equal(installing, true);
  assert.deepEqual(commands, []);
  reject(Error('invalid signature')); await first; await second;
  assert.equal(installing, false);
  assert.deepEqual(commands, [], 'failed verification must never restart');
  assert.equal(warnings.length, 1);
  const retry = install();
  assert.equal(installs, 2);
  resolve(); await retry;
  assert.deepEqual(commands, ['relaunch_after_update']);
});

test('the release upload includes the Windows updater archives referenced by its manifest', () => {
  const workflow = fs.readFileSync(path.join(__dirname, '../.github/workflows/release.yml'), 'utf8');
  const uploads = workflow.split('body_path: RELEASE_NOTES.md')[1];
  assert.ok(uploads.includes('Disc Xplorer_Windows_*.nsis.zip'));
  assert.ok(uploads.includes('validate-updater-manifest.mjs'));
});

test('published asset validation rejects a manifest with missing Windows packages', async () => {
  const { validateUpdaterManifest } = await import('../scripts/validate-updater-manifest.mjs');
  const suffixes = {
    'darwin-aarch64': 'macOS_ARM_v2.0.15.app.tar.gz', 'darwin-x86_64': 'macOS_x64_v2.0.15.app.tar.gz',
    'windows-x86_64': 'Windows_x64_v2.0.15.nsis.zip', 'windows-aarch64': 'Windows_ARM_v2.0.15.nsis.zip',
    'linux-x86_64': 'Linux_x64_v2.0.15.AppImage.tar.gz', 'linux-aarch64': 'Linux_ARM_v2.0.15.AppImage.tar.gz',
  };
  const assets = Object.values(suffixes).map(s => ({ name: `Disc.Xplorer_${s}`, size: 100 }));
  const manifest = { version: '2.0.15', platforms: Object.fromEntries(Object.entries(suffixes).map(([platform, s]) => [platform, {
    url: `https://github.com/whatever-industries/disc-xplorer/releases/download/v2.0.15/Disc.Xplorer_${s}`,
    signature: Buffer.from('untrusted comment: test\nsignature').toString('base64'),
  }])) };
  validateUpdaterManifest(manifest, assets);
  assert.throws(() => validateUpdaterManifest(manifest, assets.filter(a => !a.name.includes('Windows'))), /Missing updater asset/);
  manifest.platforms['darwin-aarch64'].url = 'https://example.com/wrong.tar.gz';
  assert.throws(() => validateUpdaterManifest(manifest, assets), /Incorrect updater URL/);
});

test('concurrent release completion selects the highest stable version for the updater', async () => {
  const { latestStableTag } = await import('../scripts/validate-updater-manifest.mjs');
  const releases = ['v2.0.14', 'v2.0.15', 'v2.0.9', 'v2.0.100'].map(tagName => ({ tagName, isDraft: false, isPrerelease: false }));
  releases.push({ tagName: 'v3.0.0', isDraft: true }, { tagName: 'v4.0.0', isPrerelease: true }, { tagName: 'v5.0.0-beta' });
  assert.equal(latestStableTag(releases), 'v2.0.100');
  assert.equal(latestStableTag(releases.slice(0, 3)), 'v2.0.15');
  assert.equal(latestStableTag(releases.slice(0, 3).reverse()), 'v2.0.15');
});

test('available update replaces the activated logo and starts installation from its tile', async () => {
  const jsx = (type, props) => ({ type, props });
  const module = { exports: {}, require(name) {
    if (name === 'react') return { useState: value => [value, () => {}], useRef: value => ({ current: value }), useEffect() {} };
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (name === './i18n') return { tr: value => value, localizeMessage: value => value };
    if (name.endsWith('.css')) return {};
    if (name === '@tauri-apps/api/core') return { invoke: async () => {} };
    if (name === '@tauri-apps/plugin-opener') return { openUrl: async () => { throw Error('Update must not open donation link'); } };
    throw Error(name);
  } };
  const compiled = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src/LaunchScreen.tsx'), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  vm.runInNewContext(compiled, module);
  const nodes = (node, result = []) => {
    if (Array.isArray(node)) node.forEach(child => nodes(child, result));
    else if (node && typeof node === 'object') { result.push(node); nodes(node.props?.children, result); }
    return result;
  };
  let installs = 0;
  const props = { logo: 'logo.png', donationUrl: 'https://example.com/donate', donationEligible: true,
    donationActivated: true, availableUpdateVersion: '2.0.15', installingUpdate: false,
    onInstallUpdate: async () => { installs++; }, settings: null, tools: null };
  const render = () => nodes(module.exports.LaunchScreen(props));
  const tile = render().find(node => node.props?.className === 'launch-update');
  assert.ok(tile);
  await tile.props.onClick();
  assert.equal(installs, 1);
  assert.equal(render().some(node => node.props?.className === 'launch-donation'), false);
  props.installingUpdate = true;
  assert.equal(render().find(node => node.props?.className === 'launch-update').props.disabled, true);
  props.availableUpdateVersion = null;
  assert.ok(render().some(node => node.props?.className?.includes('launch-icon--static')));
});

test('launch update check waits for local activation and checks once for donors', async () => {
  let previous, cleanup, checks = 0, available;
  const update = { version: '2.0.14' };
  const context = {
    IS_SECTOR_VIEW_WINDOW: false, donationActivationChecked: false, donationActivated: false,
    updateCheckStarted: { current: false },
    checkForUpdate: async () => { checks++; return update; },
    setAvailableUpdate: value => { available = value; },
    useEffect(fn, deps) {
      if (!previous || deps.some((value, i) => !Object.is(value, previous[i]))) {
        cleanup?.(); previous = deps; cleanup = fn();
      }
    },
  };
  vm.createContext(context);
  const render = () => vm.runInContext(effectJs, context);
  render(); await tick();
  assert.equal(checks, 0);
  context.donationActivationChecked = true;
  render(); await tick();
  assert.equal(checks, 0, 'unactivated users do not check');
  context.donationActivated = true;
  render(); await tick();
  assert.equal(checks, 1);
  assert.equal(available, update, 'the available update reaches the tile');
  render(); await tick();
  assert.equal(checks, 1);
  cleanup?.();
});
