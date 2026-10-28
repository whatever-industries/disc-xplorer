import fs from 'node:fs';
import { pathToFileURL } from 'node:url';

export function latestStableTag(releases) {
  const stable = releases.filter(release => !release.isDraft && !release.isPrerelease && /^v\d+\.\d+\.\d+$/.test(release.tagName));
  stable.sort((a, b) => {
    const left = a.tagName.slice(1).split('.').map(Number);
    const right = b.tagName.slice(1).split('.').map(Number);
    return right[0] - left[0] || right[1] - left[1] || right[2] - left[2];
  });
  if (!stable.length) throw Error('No published stable release');
  return stable[0].tagName;
}

export function validateUpdaterManifest(manifest, assets) {
  const suffixes = {
    'darwin-aarch64': 'macOS_ARM.app.tar.gz',
    'darwin-x86_64': 'macOS_x64.app.tar.gz',
    'windows-x86_64': 'Windows_x64.exe',
    'windows-aarch64': 'Windows_ARM.exe',
    'linux-x86_64': 'Linux_x64.AppImage.tar.gz',
    'linux-aarch64': 'Linux_ARM.AppImage.tar.gz',
  };
  if (!/^\d+\.\d+\.\d+$/.test(manifest.version)) throw Error('Invalid updater version');
  if (Object.keys(manifest.platforms ?? {}).length !== 6) throw Error('Expected six updater platforms');
  const published = new Map(assets.map(asset => [asset.name, asset]));
  for (const [platform, suffix] of Object.entries(suffixes)) {
    const entry = manifest.platforms[platform];
    const dot = suffix.indexOf('.');
    const name = `Disc.Xplorer_${suffix.slice(0, dot)}_v${manifest.version}${suffix.slice(dot)}`;
    const url = `https://github.com/whatever-industries/disc-xplorer/releases/download/v${manifest.version}/${name}`;
    if (entry?.url !== url) throw Error(`Incorrect updater URL for ${platform}`);
    const asset = published.get(name);
    if (!asset || !Number.isSafeInteger(asset.size) || asset.size <= 0) throw Error(`Missing updater asset: ${name}`);
    if (typeof entry.signature !== 'string' || !entry.signature.length) throw Error(`Missing signature for ${platform}`);
    const signature = Buffer.from(entry.signature, 'base64');
    if (signature.toString('base64') !== entry.signature || !signature.toString().startsWith('untrusted comment:')) {
      throw Error(`Invalid signature encoding for ${platform}`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
  const release = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  validateUpdaterManifest(manifest, release.assets);
  console.log('All six updater URLs match published release assets.');
}
