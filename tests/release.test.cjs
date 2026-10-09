const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const metadata = import('../scripts/release-metadata.mjs');

test('upstream update waits for all six stable binaries and their digests', async () => {
  const { targets, manifestFor } = await metadata;
  const release = { tag_name: 'b761', draft: false, prerelease: false,
    assets: targets.map(target => ({ name: `redumper-b761-${target}.zip`, state: 'uploaded', size: 123, digest: 'sha256:' + 'a'.repeat(64) })) };
  assert.equal(Object.keys(manifestFor(release).assets).length, 6);
  assert.throws(() => manifestFor({ ...release, assets: release.assets.slice(1) }), /missing/);
  assert.throws(() => manifestFor({ ...release, prerelease: true }), /stable/);
  assert.throws(() => manifestFor({ ...release, tag_name: 'b761; echo bad' }), /stable/);
  assert.throws(() => manifestFor({ ...release, assets: release.assets.map(a => ({ ...a, digest: null })) }), /missing/);
});

test('release metadata updates only the app version and all six download links', async t => {
  const { nextPatch, setVersion } = await metadata;
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'dx-release-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'src-tauri'));
  const files = {
    'package.json': JSON.stringify({ name: 'tauri-app', version: '2.0.0' }),
    'package-lock.json': JSON.stringify({ version: '2.0.0', packages: { '': { version: '2.0.0' }, dependency: { version: '1.0.0' } } }),
    'src-tauri/tauri.conf.json': JSON.stringify({ version: '2.0.0' }),
    'src-tauri/Cargo.toml': '[package]\nname = "tauri-app"\nversion = "2.0.0"\n[dependencies]\nthing = "1.0.0"\n',
    'src-tauri/Cargo.lock': '[[package]]\nname = "dependency"\nversion = "1.0.0"\n\n[[package]]\nname = "tauri-app"\nversion = "2.0.0"\n',
  };
  for (const [file, content] of Object.entries(files)) fs.writeFileSync(path.join(root, file), content);
  assert.equal(nextPatch('2.0.0'), '2.0.1');
  assert.throws(() => nextPatch('2.0.0-beta'), /stable/);
  setVersion(root, '2.0.1', 'Engine update.', { tag: 'b761', automaticRelease: '2.0.1' });
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, 'package-lock.json'))).packages.dependency.version, '1.0.0');
  assert.match(fs.readFileSync(path.join(root, 'src-tauri/Cargo.lock'), 'utf8'), /name = "dependency"\nversion = "1.0.0"/);
  assert.match(fs.readFileSync(path.join(root, 'src-tauri/Cargo.lock'), 'utf8'), /name = "tauri-app"\nversion = "2.0.1"/);
  const notes = fs.readFileSync(path.join(root, 'RELEASE_NOTES.md'), 'utf8');
  assert.equal((notes.match(/releases\/download\/v2.0.1\//g) || []).length, 6);
  assert.ok(notes.includes('Disc.Xplorer_macOS_x64_v2.0.1.zip'));
});

test('checked-in versions and pinned asset names agree', async () => {
  const { downloadTable, targets } = await metadata;
  const root = path.resolve(__dirname, '..');
  // Windows checkouts use CRLF; compare metadata content independently of EOLs.
  const read = file => fs.readFileSync(path.join(root, file), 'utf8').replace(/\r\n/g, '\n');
  const version = JSON.parse(read('package.json')).version;
  assert.equal(JSON.parse(read('package-lock.json')).version, version);
  assert.equal(JSON.parse(read('package-lock.json')).packages[''].version, version);
  assert.equal(JSON.parse(read('src-tauri/tauri.conf.json')).version, version);
  assert.ok(read('src-tauri/Cargo.toml').includes(`version = "${version}"`));
  assert.ok(read('src-tauri/Cargo.lock').includes(`name = "tauri-app"\nversion = "${version}"`));
  assert.ok(read('RELEASE_NOTES.md').endsWith(downloadTable(version)));
  const manifest = JSON.parse(read('.redumper/upstream.json'));
  for (const target of targets) assert.equal(manifest.assets[target].name, `redumper-${manifest.tag}-${target}.zip`);
});
