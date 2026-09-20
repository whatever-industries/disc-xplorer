const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const root = require('node:path').resolve(__dirname, '..');
const ts = require(root + '/node_modules/typescript');
const source = ts.createSourceFile('App.tsx', fs.readFileSync(root + '/src/App.tsx', 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const wanted = new Set(['distinctFilesystems', 'safeFileName', 'scanBx', 'scanBatch', 'startBx', 'startBatch', 'clearBx', 'clearBatch', 'extractOneDisc']);
const parts = [];
function visit(node) {
    if (ts.isFunctionDeclaration(node) && wanted.has(node.name?.text))
        parts.push(node.getText(source));
    if (ts.isVariableStatement(node) && node.declarationList.declarations.some(d => d.name.getText(source) === 'ISO_VIEWS'))
        parts.push(node.getText(source));
    ts.forEachChild(node, visit);
}
visit(source);
assert.equal(parts.length, 10);
const js = ts.transpileModule(parts.join('\n'), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText;
// Load the actual component functions without opening Tauri or a native window.
// Only IPC, React setters and storage are stubbed; production logic runs unchanged.
function harness(items = [], overrides = {}) {
    const calls = [], state = {};
    const plan = { items };
    const c = { bxPlan: plan, bxScanning: false, bxRunning: false, bxScanRef: { current: 0 }, bxReadyPlanRef: { current: plan },
        batchPlan: null, batchScanning: false, convRunning: false, batchScanRef: { current: 0 }, batchReadyPlanRef: { current: null },
        batchSrcs: ['/source'], batchOut: '/output', batchKeys: '', batchRecursive: true, batchConflict: 'rename', batchTarget: 'auto',
        fmtBytes: String, localStorage: { removeItem: () => { } }, batchLogLine: () => { }, summariseBatch: () => ({ text: 'done', failed: false }),
        runConversionJobs: async (jobs) => { state.jobs = jobs; return jobs; }, bxSrcs: ['/source'], bxOut: '/output', bxRecursive: true, bxConflict: 'rename', bxAudio: 'with', xaDefault: 'ask',
        bxCancelRef: { current: false }, extractCancelRef: { current: false }, forkModeRef: { current: 'appledouble' }, audioFormat: 'wav', gapMode: 'previous',
        invoke: async (cmd, args) => { calls.push({ cmd, args }); return null; }, ...overrides };
    for (const k of ['BxLog', 'BxSummary', 'BxRunning', 'BxStatus', 'BxPlan', 'BxScanning', 'BxError', 'BxSrcs', 'BxOut',
        'BatchPlan', 'BatchScanning', 'BatchError', 'BatchSrcs', 'BatchOut', 'BatchKeys', 'BatchSummary', 'BatchLog'])
        c['set' + k] = v => { state[k] = v; c[k[0].toLowerCase() + k.slice(1)] = v; };
    vm.createContext(c);
    vm.runInContext(js, c);
    const scan = c.scanBx;
    c.scanBx = async () => { state.rescanned = true; };
    return { c, calls, state, scan };
}
const disc = (name, extra = {}) => ({ name, path: '/source/' + name, dest_path: '/output/' + name, filesystems: ['ISO 9660', 'Joliet', 'Path Table'], audio_tracks: [], problem: null, ...extra });
function deferred() { let resolve, reject; const promise = new Promise((r, j) => { resolve = r; reject = j; }); return { promise, resolve, reject }; }
test('hybrid disc uses distinct filesystems and its own audio metadata', async () => {
    const h = harness([disc('hybrid', { filesystems: ['ISO 9660', 'Joliet', 'HFS', 'Path Table'], audio_tracks: [2, 3] })]);
    h.c.invoke = async (cmd, args) => { h.calls.push({ cmd, args }); return cmd === 'disc_cdtext' ? { tracks: { 2: { title: 'Artist/Title' } } } : null; };
    await h.c.startBx();
    assert.deepEqual(h.calls.map(x => x.cmd), ['disc_cdtext', 'save_directory', 'save_directory', 'save_audio_track', 'save_audio_track']);
    assert.ok(h.calls.every(x => (x.args.imagePath || x.args.cuePath) === '/source/hybrid'));
    assert.deepEqual(h.calls.slice(1).map(x => x.args.destPath), ['/output/hybrid/ISO 9660', '/output/hybrid/HFS', '/output/hybrid/Audio Tracks/02 - Artist-Title.wav', '/output/hybrid/Audio Tracks/Track 03.wav']);
    assert.equal(h.calls[1].args.filesystem, 'Joliet');
    assert.equal(h.state.BxSummary.failed, false);
});
test('UDF bridge extracts once and files-only excludes audio', async () => {
    const h = harness([disc('dvd', { filesystems: ['ISO 9660', 'Joliet', 'UDF 1.02'], audio_tracks: [2] })], { bxAudio: 'none' });
    await h.c.startBx();
    assert.equal(h.calls.length, 1);
    assert.equal(h.calls[0].args.filesystem, 'UDF 1.02');
});
test('audio-only omits data and places tracks in disc folder', async () => {
    const h = harness([disc('mixed', { audio_tracks: [2] })], { bxAudio: 'only' });
    await h.c.startBx();
    assert.deepEqual(h.calls.map(x => x.cmd), ['disc_cdtext', 'save_audio_track']);
    assert.equal(h.calls[1].args.destPath, '/output/mixed/Track 02.wav');
});
test('unreadable disc does not stop following discs; skipped discs never run', async () => {
    const h = harness([disc('bad'), disc('skip', { problem: 'no data' }), disc('good')]);
    h.c.invoke = async (cmd, args) => { h.calls.push({ cmd, args }); if (args.imagePath.endsWith('/bad'))
        throw Error('read failed'); };
    await h.c.startBx();
    assert.equal(h.calls.length, 2);
    assert.equal(h.calls[1].args.imagePath, '/source/good');
    assert.equal(h.state.BxSummary.failed, true);
    assert.match(h.state.BxSummary.text, /bad/);
    assert.equal(h.state.BxRunning, false);
});
test('cancel prevents subsequent discs', async () => {
    const h = harness([disc('first'), disc('second')]);
    h.c.invoke = async (cmd, args) => { h.calls.push({ cmd, args }); h.c.bxCancelRef.current = true; };
    await h.c.startBx();
    assert.equal(h.calls.length, 1);
    assert.match(h.state.BxSummary.text, /cancelled/);
});
test('newest batch scan must win when earlier request finishes last', async () => {
    const h = harness();
    const old = deferred(), fresh = deferred();
    h.c.invoke = async (cmd, args) => args.output === '/old-output' ? old.promise : fresh.promise;
    const p1 = h.scan(['/old-source'], '/old-output'), p2 = h.scan(['/new-source'], '/new-output');
    fresh.resolve({ items: [disc('new', { dest_path: '/new-output/new' })] });
    await p2;
    old.resolve({ items: [disc('old', { dest_path: '/old-output/old' })] });
    await p1;
    assert.equal(h.state.BxPlan.items[0].dest_path, '/new-output/new');
});
test('starting while a replacement scan is pending must not run an old plan', async () => {
    const h = harness([disc('old', { dest_path: '/old-output/old' })]);
    const pending = deferred();
    h.c.invoke = async (cmd, args) => { h.calls.push({ cmd, args }); return cmd === 'plan_batch_extraction' ? pending.promise : null; };
    const scan = h.scan(['/new-source'], '/new-output');
    assert.equal(h.state.BxScanning, true);
    await h.c.startBx();
    pending.resolve({ items: [disc('new')] });
    await scan;
    assert.equal(h.calls.filter(x => x.cmd === 'save_directory').length, 0);
});
test('newest conversion scan must also win when earlier request finishes last', async () => {
    const h = harness();
    const old = deferred(), fresh = deferred();
    h.c.invoke = async (cmd, args) => args.output === '/old-output' ? old.promise : fresh.promise;
    const p1 = h.c.scanBatch(['/old-source'], '/old-output'), p2 = h.c.scanBatch(['/new-source'], '/new-output');
    fresh.resolve({ items: [{ out_path: '/new-output/new.iso' }] });
    await p2;
    old.resolve({ items: [{ out_path: '/old-output/old.iso' }] });
    await p1;
    assert.equal(h.state.BatchPlan.items[0].out_path, '/new-output/new.iso');
});
for (const [prefix, cap, scanner, clear, start] of [
    ['bx', 'Bx', 'scanBx', 'clearBx', 'startBx'],
    ['batch', 'Batch', 'scanBatch', 'clearBatch', 'startBatch'],
]) {
    const scan = h => scanner === 'scanBx' ? h.scan : h.c.scanBatch;
    test(`${cap}: stale errors and finalizers cannot clear a pending newer scan`, async () => {
        const h = harness(), old = deferred(), fresh = deferred();
        h.c.invoke = async (cmd, args) => args.output === '/old' ? old.promise : fresh.promise;
        const p1 = scan(h)(['/source'], '/old'), p2 = scan(h)(['/source'], '/new');
        old.reject(Error('old failure'));
        await p1;
        assert.equal(h.state[cap + 'Scanning'], true);
        assert.equal(h.state[cap + 'Error'], null);
        fresh.resolve({ items: [disc('fresh')] });
        await p2;
        assert.equal(h.state[cap + 'Plan'].items[0].name, 'fresh');
        assert.equal(h.state[cap + 'Scanning'], false);
    });
    test(`${cap}: stale errors cannot erase a completed newer plan`, async () => {
        const h = harness(), old = deferred(), fresh = deferred();
        h.c.invoke = async (cmd, args) => args.output === '/old' ? old.promise : fresh.promise;
        const p1 = scan(h)(['/source'], '/old'), p2 = scan(h)(['/source'], '/new');
        fresh.resolve({ items: [disc('fresh')] });
        await p2;
        old.reject(Error('old failure'));
        await p1;
        assert.equal(h.state[cap + 'Plan'].items[0].name, 'fresh');
        assert.equal(h.state[cap + 'Error'], null);
    });
    for (const reset of ['clear', 'empty']) {
        test(`${cap}: ${reset} invalidates an in-flight scan`, async () => {
            const h = harness(), pending = deferred();
            h.c.invoke = () => pending.promise;
            const work = scan(h)(['/source'], '/output');
            if (reset === 'clear')
                h.c[clear]();
            else
                await scan(h)([], '/output');
            pending.resolve({ items: [disc('stale')] });
            await work;
            assert.equal(h.state[cap + 'Plan'], null);
            assert.equal(h.state[cap + 'Scanning'], false);
            assert.equal(h.c[prefix + 'ReadyPlanRef'].current, null);
        });
    }
    test(`${cap}: old render's Start handler cannot use an invalidated plan`, async () => {
        const h = harness(), pending = deferred(), old = { items: [disc('old')] };
        h.c[prefix + 'Plan'] = old;
        h.c[prefix + 'ReadyPlanRef'].current = old;
        h.c.invoke = (cmd, args) => { h.calls.push({ cmd, args }); return cmd.startsWith('plan_batch_') ? pending.promise : Promise.resolve(null); };
        const work = scan(h)(['/new-source'], '/new-output');
        // Model the handler closure still holding values from the earlier render.
        h.c[prefix + 'Plan'] = old;
        h.c[prefix + 'Scanning'] = false;
        await h.c[start]();
        assert.equal(h.calls.length, 1);
        assert.equal(h.state.jobs, undefined);
        pending.resolve({ items: [disc('new')] });
        await work;
    });
    test(`${cap}: current scan failure leaves no runnable plan`, async () => {
        const h = harness();
        h.c.invoke = async () => { throw Error('scan failure'); };
        await scan(h)(['/source'], '/output');
        assert.equal(h.state[cap + 'Plan'], null);
        assert.match(h.state[cap + 'Error'], /scan failure/);
        assert.equal(h.state[cap + 'Scanning'], false);
    });
}
test('conversion starts the current plan after scanning completes', async () => {
    const h = harness();
    const plan = { items: [{ ...disc('disc'), out_path: '/new/disc.iso', kind: 'toraw' }], bytes_needed: 10 };
    h.c.fmtBytes = String;
    h.c.invoke = async () => plan;
    await h.c.scanBatch(['/source'], '/new');
    h.c.scanBatch = async () => { };
    await h.c.startBatch();
    assert.equal(h.state.jobs.length, 1);
    assert.equal(h.state.jobs[0].outPath, '/new/disc.iso');
});
