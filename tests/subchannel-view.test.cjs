const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');
const source = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src/SubchannelView.tsx'), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
const tick = () => new Promise(resolve => setImmediate(resolve));
const q = { q: [0x41,1,1,0,0,0,0,0,2,0,null,null], channels: null, adr: 1, control: 4,
  track: '1', index: '1', relative_msf: '00:00:00', absolute_msf: '00:02:00',
  recorded_crc: null, calculated_crc: 0x2832, crc_valid: null, timing_matches: true };
const sidecar = (name, record = q) => ({ id:name, name, format: 'SBI · Q patch', records: 2, issue: null, record,
  previous_sector: 0, next_sector: 20, previous_lba:0, next_lba:20 });
function harness(read) {
  const states = [], effects = [], calls = [], jumps = [];
  let si, ei, pending, tree;
  const jsx = (type, props) => ({ type, props });
  const context = { exports: {}, require(name) {
    if (name === 'react') return {
      useState(initial) { const i = si++; if (!(i in states)) states[i] = initial;
        return [states[i], v => { states[i] = v; }]; },
      useEffect(fn, deps) { const i = ei++, prev = effects[i];
        if (!prev || deps.some((v,j) => !Object.is(v, prev.deps[j]))) pending.push(() => {
          prev?.cleanup?.(); effects[i] = { deps, cleanup: fn() };
        }); },
    };
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'fragment' };
    if (name === './i18n') return { tr: value => value, localizeMessage: value => value };
    if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => { calls.push({command,args}); return read(args); } };
    throw Error(name);
  } };
  vm.runInNewContext(source, context);
  function nodes(node, result = []) {
    if (!node || typeof node !== 'object') return result;
    if (Array.isArray(node)) { node.forEach(child => nodes(child,result)); return result; }
    result.push(node); nodes(node.props?.children, result); return result;
  }
  const text = node => typeof node === 'string' || typeof node === 'number' ? String(node)
    : Array.isArray(node) ? node.map(text).join('') : node?.props ? text(node.props.children) : '';
  return { calls, jumps, nodes: () => nodes(tree), text: () => text(tree),
    render(imagePath = '/Disc.cue', lba = 10) {
      si = ei = 0; pending = [];
      tree = context.exports.SubchannelView({ imagePath, lba, onGo: n => jumps.push(n) });
      pending.forEach(fn => fn()); return tree;
    }, dispose() { effects.forEach(e => e?.cleanup?.()); },
  };
}

test('SBI inspection shows missing CRC honestly and record navigation accepts sector zero', async () => {
  const h = harness(() => ({ disc_lba: 10, sources: [sidecar('Disc.sbi')] }));
  h.render(); await tick(); h.render();
  assert.equal(h.calls[0].command, 'read_subchannels');
  assert.match(h.text(), /41 01 01 00 00 00 00 00 02 00 — —/);
  assert.match(h.text(), /Recorded CRCNot recorded/);
  assert.match(h.text(), /Calculated CRC2832/);
  assert.doesNotMatch(h.text(), /Q checksumMatches/);
  h.nodes().find(n => n.type === 'button' && n.props.children === '◀ Record').props.onClick();
  assert.deepEqual(h.jumps,[0]); h.dispose();
});

test('multiple sidecars remain separate and a sparse gap is not filled with generated data', async () => {
  const h = harness(() => ({ disc_lba: 10, sources: [sidecar('Disc.sbi'), sidecar('Disc.lsd',null)] }));
  h.render(); await tick(); h.render();
  h.nodes().find(n => n.type === 'select').props.onChange({ target: { value: 'Disc.lsd' } });
  h.render();
  assert.match(h.text(), /No record stored for this sector/);
  assert.doesNotMatch(h.text(), /Q bytes/); h.dispose();
});

test('late sidecar reads cannot restore old images or sectors; prior bytes clear immediately', async () => {
  let resolve;
  const late = new Promise(r => { resolve = r; });
  const h = harness(({imagePath,lba}) => imagePath === '/Old.cue' ? late : ({disc_lba:lba,sources:[]}));
  h.render('/Old.cue',0); h.render('/New.cue',2); await tick(); h.render('/New.cue',2);
  resolve({disc_lba:0,sources:[sidecar('Old.sbi')]}); await tick(); h.render('/New.cue',2);
  assert.doesNotMatch(h.text(), /Old.sbi|Q bytes/);
  assert.match(h.text(), /No matching .sub/);
  h.render('/New.cue',3);
  assert.match(h.text(), /Loading/); h.dispose();
});

test('read failures are visible and a recorded checksum mismatch is descriptive', async () => {
  const h = harness(() => { throw Error('Cannot read sidecar'); });
  h.render(); await tick(); h.render(); assert.match(h.text(), /Cannot read sidecar/); h.dispose();
  const full = harness(() => ({disc_lba:10,sources:[sidecar('Disc.sub',{
    ...q,recorded_crc:0x1234,crc_valid:false,channels:Array.from({length:8},()=>Array(12).fill(0)),
  })]}));
  full.render(); await tick(); full.render();
  assert.match(full.text(), /Q checksumMismatch/);
  assert.match(full.text(), /they do not prove disc damage/);
  assert.equal(full.nodes().filter(n => n.type === 'code').length,8); full.dispose();
});

test('records outside the image remain inspectable, without navigating to a made-up sector', async () => {
  const h = harness(({recordLba}) => ({disc_lba: recordLba ?? 10, record_only: recordLba !== null,
    sources:[{...sidecar('Disc.sbi'),previous_lba:-1,previous_sector:null, next_lba:0,next_sector:0}]}));
  h.render(); await tick(); h.render();
  h.nodes().find(n => n.type === 'button' && n.props.children === '◀ Record').props.onClick();
  h.render(); await tick(); h.render();
  assert.equal(h.calls.at(-1).args.recordLba,-1);
  assert.match(h.text(), /Sidecar record only; no corresponding image sector/);
  assert.deepEqual(h.jumps,[]);
  h.nodes().find(n => n.type === 'button' && n.props.children === 'Record ▶').props.onClick();
  h.render(); await tick(); h.render();
  assert.deepEqual(h.jumps,[0]);
  assert.doesNotMatch(h.text(), /Sidecar record only/);
  h.dispose();
});

test('same-named sidecars from different directories can be selected independently', async () => {
  const h = harness(() => ({disc_lba:10, sources:[
    {...sidecar('Disc.sbi'),id:'/Images/Disc.sbi'},
    {...sidecar('Disc.sbi',null),id:'/Images/tracks/Disc.sbi'},
  ]}));
  h.render(); await tick(); h.render();
  const options = h.nodes().filter(n => n.type === 'option');
  assert.deepEqual(options.map(n => n.props.value),['/Images/Disc.sbi','/Images/tracks/Disc.sbi']);
  assert.deepEqual(options.map(n => n.props.children),['/Images/Disc.sbi','/Images/tracks/Disc.sbi']);
  h.nodes().find(n => n.type === 'select').props.onChange({target:{value:'/Images/tracks/Disc.sbi'}});
  h.render(); assert.match(h.text(), /No record stored for this sector/); h.dispose();
});

test('changing image or main-sector navigation clears a detached record selection', async () => {
  const h = harness(({recordLba}) => ({disc_lba:recordLba ?? 10, record_only:recordLba !== null,
    sources:[{...sidecar('Disc.sbi'),previous_lba:-1,previous_sector:null}]}));
  h.render(); await tick(); h.render();
  h.nodes().find(n => n.type === 'button' && n.props.children === '◀ Record').props.onClick();
  h.render(); await tick(); h.render();
  assert.equal(h.calls.at(-1).args.recordLba,-1);
  h.render('/Other.cue',10); await tick(); h.render('/Other.cue',10);
  h.render(); await tick(); h.render();
  assert.equal(h.calls.at(-1).args.recordLba,null);
  assert.doesNotMatch(h.text(), /Sidecar record only/); h.dispose();
});
