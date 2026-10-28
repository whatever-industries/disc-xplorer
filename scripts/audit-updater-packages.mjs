import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { validateUpdaterManifest } from './validate-updater-manifest.mjs';

function decode(value) {
  if (typeof value !== 'string') throw Error('Missing base64 data');
  const data = Buffer.from(value, 'base64');
  if (!data.length || data.toString('base64') !== value) throw Error('Invalid base64 data');
  return data;
}

// Minisign envelopes use native Ed25519 verification, including the signed
// trusted comment. ED signs a BLAKE2b-512 digest; Ed is the legacy raw format.
export function verifyUpdaterSignature(data, encodedSignature, encodedKey) {
  const keyLines = decode(encodedKey).toString('utf8').trimEnd().split(/\r?\n/);
  const signatureLines = decode(encodedSignature).toString('utf8').trimEnd().split(/\r?\n/);
  if (keyLines.length !== 2 || signatureLines.length !== 4
      || !keyLines[0].startsWith('untrusted comment:')
      || !signatureLines[0].startsWith('untrusted comment:')
      || !signatureLines[2].startsWith('trusted comment: ')) throw Error('Invalid Minisign envelope');
  const key = decode(keyLines[1]);
  const packet = decode(signatureLines[1]);
  const globalSignature = decode(signatureLines[3]);
  if (key.length !== 42 || packet.length !== 74 || globalSignature.length !== 64
      || !['Ed', 'ED'].includes(key.subarray(0, 2).toString())
      || !['Ed', 'ED'].includes(packet.subarray(0, 2).toString())) throw Error('Unsupported Minisign format');
  if (!key.subarray(2, 10).equals(packet.subarray(2, 10))) throw Error('Updater signing key ID mismatch');
  const publicKey = crypto.createPublicKey({ format: 'der', type: 'spki',
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), key.subarray(10)]) });
  const signature = packet.subarray(10);
  const message = packet.subarray(0, 2).toString() === 'ED'
    ? crypto.createHash('blake2b512').update(data).digest() : data;
  if (!crypto.verify(null, message, publicKey, signature)
      || !crypto.verify(null, Buffer.concat([signature, Buffer.from(signatureLines[2].slice(17))]), publicKey, globalSignature)) {
    throw Error('Updater package signature verification failed');
  }
}

export function auditUpdaterPackages(manifest, config, directory) {
  const records = Object.values(manifest.platforms).map(entry => {
    const name = new URL(entry.url).pathname.split('/').pop();
    const localName = name.replace(/^Disc\.Xplorer_/, 'Disc Xplorer_');
    const file = path.join(directory, localName);
    return { name, file, size: fs.statSync(file).size };
  });
  validateUpdaterManifest(manifest, records);
  if (manifest.version !== config.version) throw Error('Manifest version differs from the app version');
  const report = { version: manifest.version, assets: [] };
  const inspector = fileURLToPath(new URL('./inspect-updater-package.py', import.meta.url));
  for (const [platform, entry] of Object.entries(manifest.platforms)) {
    const record = records.find(record => record.name === new URL(entry.url).pathname.split('/').pop());
    const data = fs.readFileSync(record.file);
    verifyUpdaterSignature(data, entry.signature, config.plugins.updater.pubkey);
    const contents = JSON.parse(execFileSync('python3', [inspector, platform, record.file, manifest.version], { encoding: 'utf8' }));
    // AppImage's ordinary download and updater payload must be byte-identical.
    if (platform.startsWith('linux-')) {
      const raw = record.file.replace(/\.tar\.gz$/, '');
      const digest = crypto.createHash('sha256').update(fs.readFileSync(raw)).digest('hex');
      if (digest !== contents.binarySha256) throw Error(`AppImage download differs from updater payload: ${platform}`);
    }
    report.assets.push({ name: record.name, size: record.size,
      digest: 'sha256:' + crypto.createHash('sha256').update(data).digest('hex') });
    console.log(`${platform}: ${platform.startsWith('windows-') ? 'signature and NSIS installer format' : 'signature, package contents and architecture'} verified`);
  }
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
  const config = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  const report = auditUpdaterPackages(manifest, config, process.argv[4] ?? process.cwd());
  fs.writeFileSync('updater-package-audit.json', JSON.stringify(report, null, 2) + '\n');
}
