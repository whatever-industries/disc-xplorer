const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');
const compile = file => ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
const helpers = { exports: {} };
vm.runInNewContext(compile('sectorDrop.ts'), helpers);
const { sectorDropTarget } = helpers.exports;
const a = { left: 12, right: 592, top: 100, bottom: 700 };
const b = { left: 592, right: 1172, top: 100, bottom: 700 };
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve; const promise = new Promise(r => resolve = r); return { promise, resolve }; };
const sector = lba => ({ bytes: Array(2048).fill(0), lba, sector_size: 2048, total_sectors: 20, user_data_offset: 0 });

test('sector drops use physical scale and actual panel bounds, excluding toolbar and outside', () => {
  for (const scale of [1, 1.5, 2]) {
    const target = (x,y) => sectorDropTarget({ x: x * scale, y: y * scale }, scale, a, b);
    assert.equal(target(200,200), 'a');
    assert.equal(target(800,200), 'b');
    assert.equal(target(592,200), 'b');
    assert.equal(target(200,50), null);
    assert.equal(target(1200,200), null);
  }
});

function harness({ initialCompareImagePath, read, subscription } = {}) {
  const states = [], refs = [], effects = [], calls = [];
  let stateIndex, refIndex, effectIndex, pending = [], tree, listener, unlistened = 0, selection = null;
  const react = {
    useState(initial) { const i = stateIndex++; if (!(i in states)) states[i] = typeof initial === 'function' ? initial() : initial;
      return [states[i], value => { states[i] = typeof value === 'function' ? value(states[i]) : value; }]; },
    useRef(initial) { const i = refIndex++; return refs[i] ??= { current: initial }; },
    useCallback(fn) { return fn; },
    useEffect(fn, deps) { const i = effectIndex++; const prev = effects[i];
      if (!prev || deps.some((dep, j) => !Object.is(dep, prev.deps[j]))) pending.push(() => {
        prev?.cleanup?.(); effects[i] = { deps, cleanup: fn() };
      }); },
  };
  const jsx = (type, props) => ({ type, props });
  const context = { exports: {}, window: { devicePixelRatio: 2, addEventListener() {}, removeEventListener() {} }, require(name) {
    if (name === 'react') return react;
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'fragment' };
    if (name === './i18n') return { tr: (text, ...args) => text.replace(/\{(\d+)\}/g, (_, n) => args[n]), localizeMessage: x => x };
    if (name === './sectorDrop') return helpers.exports;
    if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === 'read_sector') return read ? read(args) : sector(args.lba);
    } };
    if (name === '@tauri-apps/api/webview') return { getCurrentWebview: () => ({ onDragDropEvent: async fn => {
      listener = fn; if (subscription) await subscription.promise; return () => unlistened++;
    } }) };
    if (name === '@tauri-apps/plugin-dialog') return { open: async () => { calls.push({ command: 'picker' }); return selection; } };
    throw Error(name);
  } };
  vm.runInNewContext(compile('SectorView.tsx'), context);
  function nodes(node, found = []) {
    if (!node || typeof node !== 'object') return found;
    if (Array.isArray(node)) { node.forEach(child => nodes(child, found)); return found; }
    found.push(node); nodes(node.props?.children ?? null, found); return found;
  }
  function render() {
    stateIndex = refIndex = effectIndex = 0; pending = [];
    tree = context.exports.SectorView({ imagePath: '/A.iso', onClose() {}, initialCompareImagePath });
    const panels = nodes(tree).filter(n => n.props?.ref && n.type === 'div');
    panels.forEach((panel,i) => { panel.props.ref.current = { getBoundingClientRect: () => i === 0 ? a : b }; });
    pending.forEach(fn => fn());
    return tree;
  }
  return { render, nodes: () => nodes(tree), calls, select: value => { selection = value; },
    click: text => nodes(tree).find(n => n.type === 'button' && n.props.children === text).props.onClick(),
    drop: (side, file) => listener({ payload: { type: 'drop', paths: [file], position: { x: side === 'a' ? 400 : 1600, y: 400 } } }),
    dispose: () => effects.forEach(e => e?.cleanup?.()), unlistened: () => unlistened,
  };
}

async function settle(h) { h.render(); await tick(); h.render(); await tick(); h.render(); }

test('Compare opens the picker immediately; cancel keeps single-image layout', async () => {
  const h = harness(); await settle(h);
  await h.click('Compare'); h.render();
  assert.equal(h.calls.filter(c => c.command === 'picker').length, 1);
  assert.equal(h.nodes().some(n => n.props?.className === 'sv-compare-row'), false);
  h.select('/B.iso'); await h.click('Compare'); await settle(h);
  assert.ok(h.calls.some(c => c.command === 'read_sector' && c.args.imagePath === '/B.iso'));
  assert.ok(h.nodes().some(n => n.props?.className === 'sv-compare-row'));
  h.dispose();
});

test('either pane can be replaced and late comparison reads cannot restore the old image', async () => {
  const late = deferred();
  const h = harness({ initialCompareImagePath: '/B.iso', read: args => args.imagePath === '/B.iso' ? late.promise : sector(args.lba) });
  await settle(h);
  h.drop('b', '/NewB.iso'); await settle(h);
  late.resolve({ ...sector(0), bytes: Array(2048).fill(99) }); await tick(); h.render();
  const dumps = h.nodes().filter(n => typeof n.type === 'function' && n.type.name === 'HexDump');
  assert.equal(dumps[1].props.data.bytes[0], 0);
  h.drop('a', '/NewA.iso'); await settle(h);
  assert.ok(h.calls.some(c => c.command === 'read_sector' && c.args.imagePath === '/NewA.iso'));
  assert.ok(h.nodes().some(n => n.props?.title === '/NewA.iso'));
  assert.ok(h.nodes().some(n => n.props?.title === '/NewB.iso'));
  h.dispose();
});

test('late drop-listener registration is cleaned up after the view closes', async () => {
  const subscription = deferred(); const h = harness({ subscription });
  h.render(); h.dispose(); subscription.resolve(); await tick();
  assert.equal(h.unlistened(), 1);
});
