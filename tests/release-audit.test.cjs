const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const root = path.resolve(__dirname, '..');
const auditModule = import('../scripts/audit-updater-packages.mjs');

function signer() {
  const pair = crypto.generateKeyPairSync('ed25519');
  const id = crypto.randomBytes(8);
  const key = pair.publicKey.export({ format: 'der', type: 'spki' }).subarray(-32);
  const encodedKey = Buffer.from('untrusted comment: test key\n' + Buffer.concat([Buffer.from('Ed'), id, key]).toString('base64') + '\n').toString('base64');
  return { encodedKey, sign(data, algorithm = 'ED', comment = 'timestamp:1\tfile:test') {
    const message = algorithm === 'ED' ? crypto.createHash('blake2b512').update(data).digest() : data;
    const signature = crypto.sign(null, message, pair.privateKey);
    const packet = Buffer.concat([Buffer.from(algorithm), id, signature]);
    const global = crypto.sign(null, Buffer.concat([signature, Buffer.from(comment)]), pair.privateKey);
    return Buffer.from(`untrusted comment: test signature\n${packet.toString('base64')}\ntrusted comment: ${comment}\n${global.toString('base64')}\n`).toString('base64');
  } };
}

test('package signatures verify both supported Minisign formats and reject tampering', async () => {
  const { verifyUpdaterSignature } = await auditModule;
  const key = signer(), other = signer(), data = Buffer.from('package bytes');
  for (const algorithm of ['Ed', 'ED']) {
    const signature = key.sign(data, algorithm);
    verifyUpdaterSignature(data, signature, key.encodedKey);
    assert.throws(() => verifyUpdaterSignature(Buffer.from('changed bytes'), signature, key.encodedKey), /signature verification failed/);
    assert.throws(() => verifyUpdaterSignature(data, signature, other.encodedKey), /key ID mismatch/);
    const envelope = Buffer.from(signature, 'base64').toString().replace('timestamp:1', 'timestamp:2');
    assert.throws(() => verifyUpdaterSignature(data, Buffer.from(envelope).toString('base64'), key.encodedKey), /signature verification failed/);
  }
  assert.throws(() => verifyUpdaterSignature(data, 'not base64', key.encodedKey), /base64/);
});

// Build tiny, structurally representative packages with the standard library;
// no platform executable is launched by these tests.
const makePackage = String.raw`
import io, os, plistlib, struct, sys, tarfile
filename, platform, variant = sys.argv[1:]
version = "2.0.17"
def entry(archive, name, data, mode=0o755):
    item = tarfile.TarInfo(name); item.mode = mode; item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
if platform.startswith("windows-"):
    data = bytearray(128); data[:2] = b"MZ"; struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"; data[80:92] = b"NullsoftInst"
    open(filename, "wb").write(data)
else:
    with tarfile.open(filename, "w:gz") as archive:
        if platform.startswith("linux-"):
            machine = 183 if platform.endswith("aarch64") else 62
            if variant == "architecture": machine = 1
            data = bytearray(32); data[:6] = b"\x7fELF\x02\x01"; struct.pack_into("<H", data, 18, machine)
            suffix = "ARM" if platform.endswith("aarch64") else "x64"
            name = f"Disc Xplorer_Linux_{suffix}_v{version}.AppImage"
            if variant == "nested": name = "nested/" + name
            entry(archive, name, data, 0o644 if variant == "permissions" else 0o755)
            open(filename.removesuffix(".tar.gz"), "wb").write(data)
        else:
            machine = 0x100000c if platform.endswith("aarch64") else 0x1000007
            if variant == "architecture": machine = 1
            info = {"CFBundleShortVersionString": "2.0.0" if variant == "version" else version,
                "CFBundleIdentifier": "com.jaguar.optical-disc-explorer", "CFBundleExecutable": "tauri-app"}
            entry(archive, "Disc Xplorer.app/Contents/Info.plist", plistlib.dumps(info))
            entry(archive, "Disc Xplorer.app/Contents/MacOS/tauri-app", struct.pack("<II", 0xfeedfacf, machine))
            entry(archive, "Disc Xplorer.app/Contents/_CodeSignature/CodeResources", b"test")
            if variant == "traversal": entry(archive, "../outside", b"bad")
`;

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'dx-package-audit-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test('package inspection rejects wrong architectures, versions, permissions and layouts', t => {
  const directory = fixture(t);
  const inspect = path.join(root, 'scripts/inspect-updater-package.py');
  for (const [platform, variants] of [
    ['linux-x86_64', ['architecture', 'permissions', 'nested']],
    ['darwin-aarch64', ['architecture', 'version', 'traversal']],
  ]) {
    const file = path.join(directory, platform + '.tar.gz');
    execFileSync('python3', ['-c', makePackage, file, platform, 'good']);
    execFileSync('python3', [inspect, platform, file, '2.0.17'], { stdio: 'pipe' });
    for (const variant of variants) {
      execFileSync('python3', ['-c', makePackage, file, platform, variant]);
      assert.throws(() => execFileSync('python3', [inspect, platform, file, '2.0.17'], { stdio: 'pipe' }), undefined, variant);
    }
  }
  const zip = path.join(directory, 'compressed.exe');
  fs.writeFileSync(zip, Buffer.from('PK\x03\x04compressed installer'));
  assert.throws(() => execFileSync('python3', [inspect, 'windows-x86_64', zip, '2.0.17'], { stdio: 'pipe' }));
});

test('all six signed packages pass before upload; uploaded digests must match that audit', async t => {
  const { auditUpdaterPackages } = await auditModule;
  const { validateUploadedAssets } = await import('../scripts/validate-updater-manifest.mjs');
  const directory = fixture(t), key = signer();
  const suffixes = { 'darwin-aarch64': 'macOS_ARM.app.tar.gz', 'darwin-x86_64': 'macOS_x64.app.tar.gz',
    'windows-x86_64': 'Windows_x64.exe', 'windows-aarch64': 'Windows_ARM.exe',
    'linux-x86_64': 'Linux_x64.AppImage.tar.gz', 'linux-aarch64': 'Linux_ARM.AppImage.tar.gz' };
  const manifest = { version: '2.0.17', pub_date: '2026-10-27T00:00:00Z', platforms: {} };
  for (const [platform, suffix] of Object.entries(suffixes)) {
    const dot = suffix.indexOf('.');
    const name = `Disc Xplorer_${suffix.slice(0, dot)}_v2.0.17${suffix.slice(dot)}`;
    const file = path.join(directory, name);
    execFileSync('python3', ['-c', makePackage, file, platform, 'good']);
    manifest.platforms[platform] = { url: 'https://github.com/whatever-industries/disc-xplorer/releases/download/v2.0.17/' + name.replace('Disc Xplorer_', 'Disc.Xplorer_'), signature: key.sign(fs.readFileSync(file)) };
  }
  const config = { version: '2.0.17', plugins: { updater: { pubkey: key.encodedKey } } };
  const report = auditUpdaterPackages(manifest, config, directory);
  assert.equal(report.assets.length, 6);
  validateUploadedAssets(manifest, report.assets, report);
  const damaged = report.assets.map((asset, i) => i ? asset : { ...asset, digest: 'sha256:' + '0'.repeat(64) });
  assert.throws(() => validateUploadedAssets(manifest, damaged, report), /Uploaded bytes differ/);
  assert.throws(() => auditUpdaterPackages(manifest, { ...config, version: '2.0.18' }, directory), /app version/);
});

test('release verification gates publication, with uploads staged as a draft', () => {
  const workflow = fs.readFileSync(path.join(root, '.github/workflows/release.yml'), 'utf8');
  const before = workflow.indexOf('name: Verify signed updater packages before upload');
  const staging = workflow.indexOf('name: Stage GitHub Release');
  const uploaded = workflow.indexOf('name: Verify staged updater assets');
  const publish = workflow.indexOf('name: Publish verified release');
  assert.ok(before >= 0 && before < staging && staging < uploaded && uploaded < publish);
  assert.ok(workflow.slice(staging, uploaded).includes('draft: true'));
  assert.ok(workflow.slice(uploaded, publish).includes('updater-package-audit.json'));
});
