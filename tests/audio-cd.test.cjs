const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');

const source = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src/AudioCdTools.tsx'), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2021, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
const tick = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const state = [], refs = [], effects = [], calls = [];
  let si, ri, ei, queued, saved;
  const react = {
    useState(value) {
      const i = si++;
      if (!(i in state)) state[i] = typeof value === 'function' ? value() : value;
      return [state[i], value => { state[i] = typeof value === 'function' ? value(state[i]) : value; }];
    },
    useRef(value) { const i = ri++; return refs[i] ??= { current: value }; },
    useEffect(fn, deps) {
      const i = ei++, previous = effects[i];
      if (!previous || deps.some((dep, index) => !Object.is(dep, previous.deps[index]))) {
        queued.push(() => { previous?.cleanup?.(); effects[i] = { deps, cleanup: fn() }; });
      }
    },
  };
  const jsx = (type, props) => ({ type, props });
  const context = { exports: {}, require(name) {
    if (name === 'react') return react;
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'fragment' };
    if (name === './i18n') return { tr: value => value };
    if (name === '@tauri-apps/plugin-dialog') return {};
    if (name === '@tauri-apps/api/core') return { invoke: (name, args) =>
      new Promise((resolve, reject) => calls.push({ name, args, resolve, reject })) };
    throw Error(name);
  }, localStorage: { getItem: () => saved, setItem: (_key, value) => { saved = value; } } };
  vm.runInNewContext(source, context);
  const api = context.exports;
  const render = (image = '/Album.cue', preferences = { lookupOnOpen: false, frontCover: false }) => {
    si = ri = ei = 0;
    queued = [];
    const view = api.useAudioCdTools(image, false, null, [{ track_number: 1, is_data: false }], null, preferences, true);
    queued.forEach(run => run());
    return view;
  };
  return { render, api, calls, setSaved: value => { saved = value; } };
}

test('album lookup is opt-in, runs once per open, and ignores results from a previous disc', async () => {
  const { render, api, calls, setSaved } = harness();
  assert.deepEqual(JSON.parse(JSON.stringify(api.readAudioCdPreferences())), { lookupOnOpen: false, frontCover: false });
  setSaved('{"lookupOnOpen":"true","frontCover":1}');
  assert.deepEqual(JSON.parse(JSON.stringify(api.readAudioCdPreferences())), { lookupOnOpen: false, frontCover: false });
  const auto = { lookupOnOpen: true, frontCover: false };
  render();
  assert.equal(calls.length, 0);
  render('/A.cue', auto);
  render('/A.cue', auto);
  render('/B.cue', auto);
  assert.deepEqual(calls.map(c => c.args.imagePath), ['/A.cue', '/B.cue']);
  calls[0].resolve([{}]);
  await tick();
  assert.equal(render('/B.cue', auto).releases, 0);
  calls[1].resolve([{}, {}]);
  await tick();
  assert.equal(render('/B.cue', auto).releases, 2);
});

test('approval stays on its source and switching images cancels manual CTDB verification', async () => {
  const { render, calls } = harness();
  render('/A.cue').showAlbum();
  assert.equal(calls[0].name, 'lookup_audio_releases');
  const modal = render('/A.cue').dialogs.props.children[0];
  modal.props.onApply({ fields: { album: 'Album A', artist: 'Album Artist',
    tracks: [{ number: 1, title: 'Song', artist: 'Artist' }] }, cover: null });
  const current = render('/A.cue');
  assert.equal(current.tagsForTrack(1).album, 'Album A');
  current.startVerification();
  assert.equal(calls[1].name, 'verify_audio_rip');
  assert.equal(calls[1].args.imagePath, '/A.cue');
  assert.equal(render('/B.cue').tagsForTrack(1), null);
  assert.equal(calls[2].name, 'cancel_audio_verification');
  assert.equal(calls[2].args.requestId, calls[1].args.requestId);
  calls[1].resolve({ message: 'Verified' });
  await tick();
  assert.equal(Boolean(render('/B.cue').dialogs.props.children[1]), false);
});
