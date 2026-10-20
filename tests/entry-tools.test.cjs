const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');
const tick = () => new Promise(resolve => setImmediate(resolve));
function harness(kind = 'hashes') {
  const states = [], effects = [], calls = [], refs = [];
  let si = 0, ri = 0, tree, channel, resolve, reject, copied, closed = 0;
  const promise = new Promise((a,b) => { resolve = a; reject = b; });
  const request = { imagePath: '/disc.cue', filePath: '/FILE.XA', filesystem: 'ISO 9660', kind, isXa: true, xaMode: 1 };
  const react = {
    useState(value) { const i = si++; if (!(i in states)) states[i] = value;
      return [states[i], v => { states[i] = typeof v === 'function' ? v(states[i]) : v; }]; },
    useRef(value) { const i = ri++; return refs[i] ??= { current: value }; },
    useEffect(fn) { if (!effects.length) effects.push({ fn }); },
  };
  const jsx = (type, props) => ({ type, props });
  const context = { exports: {}, queueMicrotask, crypto: { randomUUID: () => 'job-123' }, navigator: { clipboard: { writeText: async s => { copied = s; } } }, require(name) {
    if (name === 'react') return react;
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (name === './i18n') return { tr: (text, ...args) => text.replace(/\{(\d+)\}/g, (_,n) => args[n]), localizeMessage: String };
    if (name === '@tauri-apps/api/core') return { Channel: class { constructor() { channel = this; } }, invoke(name,args) {
      calls.push({ name, args }); return ['sector_location','calculate_file_hashes'].includes(name) ? promise : Promise.resolve();
    } };
    throw Error(name);
  } };
  vm.runInNewContext(ts.transpileModule(fs.readFileSync(path.join(__dirname,'../src/EntryTools.tsx'),'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  function render() { si = 0; ri = 0; tree = context.exports.EntryTools({ request, onClose: () => closed++ }); }
  function nodes(node) { if (Array.isArray(node)) return node.flatMap(n => nodes(n));
    if (!node || typeof node !== 'object') return []; return [node, ...nodes(node.props?.children)]; }
  function text(node) { if (Array.isArray(node)) return node.map(text).join('');
    if (node == null || typeof node === 'boolean') return ''; return typeof node === 'object' ? text(node.props?.children) : String(node); }
  render();
  return { render, text: () => text(tree), calls, effects, nodes: () => nodes(tree), resolve, reject, request,
    start() { effects[0].cleanup = effects[0].fn(); },
    progress(n) { channel.onmessage(n); render(); },
    button(label) { return nodes(tree).find(n => n.type === 'button' && text(n) === label); },
    get copied() { return copied; }, get closed() { return closed; },
  };
}
test('file hashes use selected XA format, stream progress, expose cancellation and copy results', async () => {
  const h = harness(); h.start(); await tick();
  assert.equal(h.calls[0].name,'calculate_file_hashes'); assert.equal(h.calls[0].args.xaMode,1);
  assert.equal(h.calls[0].args.filePath,'/FILE.XA'); h.progress(12345);
  assert.match(h.text(),/12,345 bytes read/); assert.match(h.text(),/Subheader/);
  await h.button('Cancel').props.onClick(); await tick();
  assert.equal(h.calls.at(-1).name,'cancel_file_hashes'); assert.equal(h.calls.at(-1).args.jobId,'job-123');
  h.resolve({ bytes:12345, crc32:'01234567', md5:'md5-value', sha1:'sha1-value', sha256:'sha256-value' });
  await tick(); h.render(); assert.ok(h.button('Copy hashes')); assert.ok(h.button('Close'));
  await h.button('Copy hashes').props.onClick(); await tick(); assert.match(h.copied,/SHA-256: sha256-value/); assert.match(h.copied,/Format: Subheader/);
});
test('Strict Mode effect replay starts one hash job and unmount cancels it', async () => {
  const h = harness(); h.start(); h.effects[0].cleanup(); h.start(); await tick();
  assert.equal(h.calls.filter(c => c.name === 'calculate_file_hashes').length,1);
  h.effects[0].cleanup(); await tick(); assert.equal(h.calls.at(-1).name,'cancel_file_hashes');
});
test('sector location displays exact range/offset and opens the mapped viewer LBA', async () => {
  const h = harness('location'); h.start(); await tick(); assert.equal(h.calls[0].name,'sector_location');
  h.resolve({ is_directory:true, logical_block_size:2048, declared_bytes:2051, sector_count:2, viewer_lba:32,
    ranges:[{start_lba:32,end_lba:33,sector_count:2,start_byte_offset:0,end_byte_offset:2}] });
  await tick(); h.render(); assert.match(h.text(),/child files and folders are stored separately/);
  assert.match(h.text(),/End byte offset/); assert.equal(h.button('Cancel'),undefined);
  h.button('View in Sector View').props.onClick(); await tick();
  assert.equal(h.calls.at(-1).args.lba,32); assert.equal(h.calls.at(-1).name,'open_sector_view_window');
  assert.equal(h.calls.filter(c => c.name === 'calculate_file_hashes').length,0);
});
test('hash errors offer Close without displaying a checksum', async () => {
  const h = harness(); h.start(); await tick(); h.reject('Read error: truncated image'); await tick(); h.render();
  assert.match(h.text(),/truncated image/); assert.equal(h.button('Copy hashes'),undefined); h.button('Close').props.onClick(); assert.equal(h.closed,1);
});

test('actual file-list menu offers sector location and limits hashing to files', () => {
  const source = fs.readFileSync(path.join(__dirname, '../src/App.tsx'), 'utf8');
  const ast = ts.createSourceFile('App.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const wanted = new Set(['sectorLocationAction','entryToolActions']); const functions = [];
  function collect(node) {
    if (ts.isFunctionDeclaration(node) && wanted.has(node.name?.text)) functions.push(node.getText(ast));
    ts.forEachChild(node, collect);
  }
  collect(ast); assert.equal(functions.length,2);
  let request;
  const context = { imagePath:'/disc.cue', currentPath:'/DATA', activeFilesystem:'ISO 9660',
    setEntryTool: value => { request = value; }, withXaChoice: (_count,run) => run(0) };
  vm.runInNewContext(ts.transpileModule(functions.join('\n'), {
    compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022},
  }).outputText,context);
  const folder = context.entryToolActions({name:'FOLDER',is_dir:true});
  assert.ok(folder.some(item => item.label === 'Sector Location…'));
  assert.equal(folder.some(item => item.label === 'Calculate Hashes…'),false);
  folder.find(item => item.label === 'Sector Location…').run(); assert.equal(request.filePath,'/DATA/FOLDER');
  const file = context.entryToolActions({name:'FILE.BIN',is_dir:false});
  assert.ok(file.some(item => item.label === 'Sector Location…'));
  file.find(item => item.label === 'Calculate Hashes…').run(); assert.equal(request.kind,'hashes');
  assert.equal(request.filePath,'/DATA/FILE.BIN');
  context.activeFilesystem = 'HFS';
  assert.ok(context.entryToolActions({name:'FILE',is_dir:false}).some(item => item.label === 'Sector Location…'));
  assert.ok(context.entryToolActions({name:'FOLDER',is_dir:true}).some(item => item.label === 'Sector Location…'));
});

test('each hash copies just its value and empty files retain their standard digests', async () => {
  const h = harness(); h.start(); await tick();
  const empty = { bytes:0,crc32:'00000000',md5:'d41d8cd98f00b204e9800998ecf8427e',
    sha1:'da39a3ee5e6b4b0d3255bfef95601890afd80709',sha256:'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855' };
  h.resolve(empty); await tick(); h.render(); assert.match(h.text(),/Empty file/);
  for (const key of ['crc32','md5','sha1','sha256']) {
    await h.button(empty[key]).props.onClick(); await tick(); h.render();
    assert.equal(h.copied,empty[key]); assert.equal(h.button(empty[key]).props.title,'Copied');
  }
});
