const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');

// Exercise the actual drop handler, with native lookup and opening mocked.
const source = ts.createSourceFile('App.tsx', fs.readFileSync(require.resolve('../src/App.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let handler;
function visit(node) {
  if (ts.isFunctionDeclaration(node) && node.name?.text === 'handleDrop') handler = node.getText(source);
  ts.forEachChild(node, visit);
}
visit(source);
assert.ok(handler);
const code = ts.transpileModule(handler, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;

function harness(cue) {
  const opened = [], lookups = [], errors = [];
  let notice = false;
  const context = {
    invoke: async (command, args) => {
      assert.equal(command, 'find_cue_for_bin');
      lookups.push(args.binPath);
      if (cue instanceof Error) throw cue;
      return cue;
    },
    openImageAtPath: async path => opened.push(path),
    setMissingCueNotice: value => { notice = value; },
    setError: value => errors.push(value),
  };
  vm.runInNewContext(code, context);
  return { drop: context.handleDrop, opened, lookups, errors, notice: () => notice };
}

test('dropping a BIN opens its matching CUE, including a later track', async () => {
  for (const bin of ['/Disc/Game.bin', '/Disc/Game (Track 02).BIN']) {
    const h = harness('/Disc/Game.cue');
    await h.drop([bin]);
    assert.deepEqual(h.lookups, [bin]);
    assert.deepEqual(h.opened, ['/Disc/Game.cue']);
    assert.equal(h.notice(), false);
  }
});

test('missing CUE shows the notice without replacing the current image', async () => {
  const h = harness(null);
  await h.drop(['/Disc/Game.bin']);
  assert.equal(h.notice(), true);
  assert.deepEqual(h.opened, []);
});

test('dropping the CUE and its tracks together opens the CUE only once', async () => {
  const h = harness(null);
  await h.drop(['/Disc/Game (Track 02).bin', '/Disc/Game.cue', '/Disc/Game (Track 01).bin']);
  assert.deepEqual(h.opened, ['/Disc/Game.cue']);
  assert.deepEqual(h.lookups, []);
});

test('lookup failure reports an error without opening a bare BIN', async () => {
  const h = harness(new Error('Lookup failed'));
  await h.drop(['/Disc/Game.bin']);
  assert.deepEqual(h.opened, []);
  assert.equal(h.notice(), false);
  assert.match(h.errors[0], /Lookup failed/);
});
