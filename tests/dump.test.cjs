const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');
const code = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src/dump.ts'), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const deferred = () => { let resolve, reject; const promise = new Promise((r, j) => { resolve = r; reject = j; }); return { promise, resolve, reject }; };
const tick = () => new Promise(resolve => setImmediate(resolve));
const job = (id, revision, status = 'running') => ({ id, revision, status, drive: 'disk4' });

function harness(invoke = async () => null, subscribe) {
  const states = [], refs = [], cleanups = [];
  let stateIndex = 0, refIndex = 0, mounted = false, listener, offCount = 0;
  const react = {
    useState(value) {
      const i = stateIndex++;
      if (!mounted) states[i] = typeof value === 'function' ? value() : value;
      return [states[i], v => { states[i] = typeof v === 'function' ? v(states[i]) : v; }];
    },
    useRef(value) { const i = refIndex++; if (!mounted) refs[i] = { current: value }; return refs[i]; },
    useCallback(fn) { return fn; },
    useEffect(fn) { if (!mounted) cleanups.push(fn()); },
  };
  const context = { exports: {}, require(name) {
    if (name === 'react') return react;
    if (name.endsWith('/core')) return { invoke };
    if (name.endsWith('/event')) return { listen: async (_, cb) => { listener = cb; if (subscribe) await subscribe.promise; return () => offCount++; } };
    throw Error(name);
  } };
  vm.runInNewContext(code, context);
  return { api: context.exports, render() { stateIndex = refIndex = 0; const result = context.exports.useDiscDump(true); mounted = true; return result; },
    emit(payload) { listener({ payload }); }, dispose() { cleanups.forEach(f => f?.()); }, get offCount() { return offCount; } };
}

test('late snapshots and start responses cannot rewind a newer job', () => {
  const { newerDump } = harness().api;
  const final = job(2, 10, 'completed');
  assert.equal(newerDump(final, job(2, 0)), final);
  assert.equal(newerDump(final, job(1, 100)), final);
  assert.equal(newerDump(final, null), final);
  assert.equal(newerDump(final, job(3, 0)).id, 3);
});

test('initial state request cannot overwrite an event received during loading', async () => {
  const initial = deferred();
  const h = harness(() => initial.promise);
  h.render();
  await tick();
  h.emit(job(1, 4));
  initial.resolve(job(1, 0));
  await tick();
  const result = h.render();
  assert.equal(result.job.revision, 4);
  assert.equal(result.ready, true);
  h.dispose();
  assert.equal(h.offCount, 1);
});

test('unmount before subscription resolves removes the listener without loading', async () => {
  const subscription = deferred();
  let invokes = 0;
  const h = harness(async () => { invokes++; }, subscription);
  h.render(); h.dispose(); subscription.resolve();
  await tick();
  assert.equal(h.offCount, 1);
  assert.equal(invokes, 0);
});

test('double start is blocked and cancellation reserves the drive until termination', async () => {
  const start = deferred();
  const calls = [];
  const h = harness(async (command, args) => {
    calls.push({ command, args });
    if (command === 'start_redumper_dump') return start.promise;
    return null;
  });
  h.render(); await tick();
  const ready = h.render();
  let prepared = 0;
  const pending = ready.start({ drive: 'disk4' }, () => prepared++);
  await ready.start({ drive: 'disk4' }, () => prepared++);
  assert.equal(prepared, 1);
  assert.equal(ready.isDriveReserved('disk4'), true);
  assert.equal(ready.isDriveReserved('disk5'), false);
  h.emit(job(1, 2));
  start.resolve(job(1, 0)); await pending;
  assert.equal(h.render().job.revision, 2);
  await h.render().stop();
  assert.equal(h.render().running, true);
  h.emit(job(1, 3, 'stopping'));
  assert.equal(h.render().isDriveReserved('disk4'), true);
  h.emit(job(1, 4, 'cancelled'));
  assert.equal(h.render().running, false);
  assert.equal(h.render().isDriveReserved('disk4'), false);
  assert.equal(calls.filter(c => c.command === 'start_redumper_dump').length, 1);
  assert.equal(calls.find(c => c.command === 'cancel_redumper_dump').args.id, 1);
});

test('start failure releases the reservation and reports the error', async () => {
  const h = harness(async command => { if (command === 'start_redumper_dump') throw Error('folder exists'); return null; });
  h.render(); await tick();
  await h.render().start({ drive: 'disk4' }, () => {});
  assert.equal(h.render().running, false);
  assert.equal(h.render().isDriveReserved('disk4'), false);
  assert.match(h.render().error, /folder exists/);
});

test('names and paths remain portable and elapsed time freezes at completion', () => {
  const { dumpName, nameError, dumpOutput, elapsedDump } = harness().api;
  assert.equal(dumpName('Game/Disc: 1'), 'Game_Disc_ 1');
  assert.equal(nameError('日本語 (Disc 1)'), null);
  for (const name of ['..', '../disc', 'CON', 'nul.txt', 'disc.', 'disc ', 'a\\b', 'C:disc', '']) assert.ok(nameError(name), name);
  assert.equal(dumpOutput('C:\\Dumps\\', 'Disc'), 'C:\\Dumps\\Disc');
  assert.equal(dumpOutput('/', 'Disc'), '/Disc');
  assert.equal(elapsedDump({ started_at: 1000, finished_at: 62000 }, 900000), '1:01');
});

test('display paths hide Windows extended prefixes without changing other path forms', () => {
  const { displayDumpPath } = harness().api;
  assert.equal(displayDumpPath(String.raw`\\?\C:\Users\benji\Downloads\Titus`), String.raw`C:\Users\benji\Downloads\Titus`);
  assert.equal(displayDumpPath(String.raw`\\?\UNC\server\share\Titus`), String.raw`\\server\share\Titus`);
  for (const path of [String.raw`C:\Dumps\Titus`, String.raw`\\server\share\Titus`,
    String.raw`\\?\Volume{abc}\Titus`, String.raw`\\.\D:`, '/Users/jaguar/Downloads/Titus']) {
    assert.equal(displayDumpPath(path), path);
  }
});

test('refine requires approval and a current eligible job, and reserves its original drive', async () => {
  const pending = deferred();
  const calls = [];
  const h = harness(async (command, args) => {
    calls.push({ command, args });
    if (command === 'refine_redumper_dump') return pending.promise;
    return null;
  });
  h.render(); await tick();
  h.emit({ ...job(4, 2, 'warning'), can_refine: true });
  let prepared = 0;
  await h.render().refine(4, false, () => prepared++);
  await h.render().refine(3, true, () => prepared++);
  assert.equal(prepared, 0);
  const run = h.render().refine(4, true, () => prepared++, "redumper disc --retries=7");
  await h.render().refine(4, true, () => prepared++);
  assert.equal(prepared, 1);
  assert.equal(h.render().isDriveReserved('disk4'), true);
  assert.equal(calls.filter(c => c.command === 'refine_redumper_dump').length, 1);
  assert.equal(calls.at(-1).args.id, 4);
  assert.equal(calls.at(-1).args.overwriteConfirmed, true);
  assert.equal(calls.at(-1).args.manualCommand, "redumper disc --retries=7");
  h.emit({ ...job(5, 3), can_refine: false });
  pending.resolve(job(5, 0)); await run;
  assert.equal(h.render().job.revision, 3);
  h.emit({ ...job(5, 4, 'completed'), can_refine: false });
  await h.render().refine(5, true, () => prepared++);
  assert.equal(prepared, 1);
  assert.equal(h.render().isDriveReserved('disk4'), false);
});

test('refresh replaces stale recovery actions, without rewinding a newer job', async () => {
  const snapshot = deferred();
  let calls = 0;
  const h = harness(async command => {
    assert.equal(command, 'get_dump_job');
    return ++calls === 1 ? null : snapshot.promise;
  });
  h.render(); await tick();
  h.emit({ ...job(1, 5, 'cancelled'), can_refine: true, output_exists: true });
  const refresh = h.render().refresh();
  snapshot.resolve({ ...job(1, 6, 'cancelled'), can_refine: false, output_exists: false });
  await refresh;
  assert.equal(h.render().job.can_refine, false);
  assert.equal(h.render().job.output_exists, false);
  h.emit({ ...job(2, 0), can_refine: false });
  await h.render().refresh();
  assert.equal(h.render().job.id, 2);
});

test('dump settings default to permissive modes and verification off, preserving explicit choices', () => {
  const source = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src/DumpSettings.tsx'), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  let saved = null;
  const context = { exports: {}, require: () => ({}), localStorage: { getItem: () => saved } };
  vm.runInNewContext(source, context);
  const read = () => JSON.parse(JSON.stringify(context.exports.readDumpOptions()));
  const defaults = { advanced_command: false, any_drive: true, complete_with_errors: true, double_dump: false,
    correct_offset_shift: true, retries: 0, log_archive: "7z", auto_eject: false, skeleton: true, rings: false,
    verbose: false, refine_subchannel: false, refine_sector_mode: false, dvd_raw: false, bd_raw: false };
  assert.deepEqual(read(), defaults);
  saved = '{broken'; assert.deepEqual(read(), defaults);
  saved = 'null'; assert.deepEqual(read(), defaults);
  saved = JSON.stringify({ any_drive: false, complete_with_errors: false, double_dump: true });
  assert.deepEqual(read(), { ...defaults, any_drive: false, complete_with_errors: false, double_dump: true });
  saved = JSON.stringify({ correct_offset_shift: false, retries: 5, skeleton: true, dvd_raw: true });
  assert.deepEqual(read(), { ...defaults, correct_offset_shift: false, retries: 5, skeleton: true, dvd_raw: true });
  for (const retries of [-1, 10001, 1.5, "5"]) {
    saved = JSON.stringify({ retries }); assert.equal(read().retries, 0);
  }
  saved = JSON.stringify({ advanced_command: true }); assert.equal(read().advanced_command, true);
  saved = JSON.stringify({ advanced_command: 'true' }); assert.equal(read().advanced_command, false);
  saved = JSON.stringify({ log_archive: "off" }); assert.equal(read().log_archive, "off");
  saved = JSON.stringify({ log_archive: "invalid" }); assert.equal(read().log_archive, "7z");
  saved = JSON.stringify({ any_drive: 'false', double_dump: 'true' });
  assert.deepEqual(read(), defaults);
});

test('existing dump refinement requires confirmation and uses the normal drive reservation', async () => {
  const pending = deferred(), calls = [];
  const h = harness(async (command, args) => {
    calls.push({ command, args });
    return command === 'refine_existing_redumper_dump' ? pending.promise : null;
  });
  h.render(); await tick();
  const request = { drive: 'disk14', name: 'Disc', output_parent: '/dumps' };
  await h.render().refineExisting(request, false, () => { throw Error('must not prepare'); });
  assert.equal(calls.filter(c => c.command === 'refine_existing_redumper_dump').length, 0);
  const started = h.render().refineExisting(request, true, () => {});
  assert.equal(h.render().isDriveReserved('disk14'), true);
  await h.render().refineExisting(request, true, () => { throw Error('duplicate'); });
  assert.equal(calls.filter(c => c.command === 'refine_existing_redumper_dump').length, 1);
  pending.resolve({ ...job(1, 0), drive: 'disk14' });
  await started;
  assert.equal(h.render().running, true);
});
