const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');
const { test } = require('node:test');

const script = import('../scripts/check-redumper.mjs');
const metadata = import('../scripts/release-metadata.mjs');
const repository = 'whatever-industries/disc-xplorer';
const pinned = {
  'flac-bound': '0.5.0',
  'libflac-sys': '0.3.4',
  'mp3lame-encoder': '0.3.0',
  'mp3lame-sys': '0.2.0',
  'unrelated': '1.0.0',
  'tauri-app': '2.1.6',
};

const readLock = versions => Object.entries(versions).map(
  ([name, version]) => `[[package]]\nname = "${name}"\nversion = "${version}"\n`,
).join('\n');

async function checkout(t, automaticRelease = null) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'dx-dependency-update-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'src-tauri'));
  fs.mkdirSync(path.join(root, '.redumper'));
  const { targets, manifestFor } = await metadata;
  const release = tag => ({
    tag_name: tag, draft: false, prerelease: false,
    assets: targets.map(target => ({
      name: `redumper-${tag}-${target}.zip`, state: 'uploaded', size: 123, digest: 'sha256:' + 'a'.repeat(64),
    })),
  });
  const manifest = manifestFor(release('b773'), automaticRelease);
  const files = {
    '.redumper/upstream.json': JSON.stringify(manifest),
    'package.json': JSON.stringify({ version: pinned['tauri-app'] }),
    'package-lock.json': JSON.stringify({ packages: { '': {} } }),
    'src-tauri/tauri.conf.json': '{}',
    'src-tauri/Cargo.toml': '[package]\nname = "tauri-app"\nversion = "2.1.6"\n',
    'src-tauri/Cargo.lock': readLock(pinned),
  };
  for (const [name, contents] of Object.entries(files)) fs.writeFileSync(path.join(root, name), contents);
  const read = name => fs.readFileSync(path.join(root, name), 'utf8');
  const update = versions => () => fs.writeFileSync(path.join(root, 'src-tauri/Cargo.lock'), readLock({ ...pinned, ...versions }));
  const latest = tag => endpoint => {
    assert.equal(endpoint, 'repos/superg/redumper/releases/latest');
    return release(tag);
  };
  return { root, read, update, latest, manifest };
}

test('a compatible encoder update preserves the pinned redumper binaries', async t => {
  const { root, read, update, latest, manifest } = await checkout(t);
  const { prepareUpdate } = await script;
  const result = prepareUpdate(root, repository, latest('b773'), update({ 'libflac-sys': '0.3.5' }));
  assert.deepEqual(result, { changed: true, release: true, tag: 'v2.1.7', redumper: false, flac: true, mp3: false });
  assert.deepEqual(JSON.parse(read('.redumper/upstream.json')), { ...manifest, automaticRelease: '2.1.7' });
  assert.match(read('src-tauri/Cargo.lock'), /name = "libflac-sys"\nversion = "0.3.5"/);
  assert.match(read('RELEASE_NOTES.md'), /^Updated the built-in FLAC encoder\./);
});

test('an unrelated lockfile refresh does not create a release', async t => {
  const { root, read, update, latest } = await checkout(t);
  const { prepareUpdate } = await script;
  const result = prepareUpdate(root, repository, latest('b773'), update({ unrelated: '1.2.0' }));
  assert.equal(result.release, false);
  assert.equal(read('src-tauri/Cargo.lock'), readLock(pinned));
  assert.equal(JSON.parse(read('package.json')).version, '2.1.6');
});

test('redumper and encoder updates use one app version and release', async t => {
  const { root, read, update, latest } = await checkout(t);
  const { prepareUpdate } = await script;
  const result = prepareUpdate(root, repository, latest('b774'), update({ 'mp3lame-encoder': '0.3.1', 'libflac-sys': '0.3.5' }));
  assert.deepEqual(result, { changed: true, release: true, tag: 'v2.1.7', redumper: true, flac: true, mp3: true });
  assert.equal(JSON.parse(read('.redumper/upstream.json')).tag, 'b774');
  assert.match(read('RELEASE_NOTES.md'), /^- Updated the bundled redumper.*\n- Updated the built-in FLAC and MP3 encoders\./);
});

test('a missing or draft automatic release retries the verified version before looking for updates', async t => {
  const { root, read } = await checkout(t, '2.1.6');
  const { prepareUpdate } = await script;
  for (const release of [null, { draft: true }]) {
    const result = prepareUpdate(root, repository, endpoint => {
      assert.equal(endpoint, `repos/${repository}/releases/tags/v2.1.6`);
      if (release) return release;
      throw Object.assign(new Error('Release not found'), { stderr: 'gh: Not Found (HTTP 404)' });
    }, () => assert.fail('Cargo ran before pending release retried'));
    assert.deepEqual(result, { changed: false, release: true, tag: 'v2.1.6', redumper: false, flac: false, mp3: false });
    assert.equal(read('src-tauri/Cargo.lock'), readLock(pinned));
  }
});

test('an encoder downgrade fails the update without advancing the app version', async t => {
  const { root, read, update, latest } = await checkout(t);
  const { prepareUpdate } = await script;
  assert.throws(() => prepareUpdate(root, repository, latest('b773'), update({ 'libflac-sys': '0.3.3' })), /downgrade libflac-sys/);
  assert.equal(JSON.parse(read('package.json')).version, '2.1.6');
});
