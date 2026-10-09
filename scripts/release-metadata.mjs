import fs from "node:fs";
import path from "node:path";

export const targets = ["macos-arm64", "macos-x64", "windows-x64", "windows-arm64", "linux-x64", "linux-arm64"];
const platforms = [
  ["macOS (Apple Silicon)", "macOS_ARM", "zip"],
  ["macOS (Intel)", "macOS_x64", "zip"],
  ["Windows (x64)", "Windows_x64", "exe"],
  ["Windows (ARM)", "Windows_ARM", "exe"],
  ["Linux (x64)", "Linux_x64", "AppImage"],
  ["Linux (ARM)", "Linux_ARM", "AppImage"],
];

export function downloadTable(version) {
  return "## Download\n\n| Platform |\n|----------|\n" + platforms.map(([label, suffix, ext]) => {
    const file = `Disc.Xplorer_${suffix}_v${version}.${ext}`;
    return `| [**${label}**](https://github.com/whatever-industries/disc-xplorer/releases/download/v${version}/${file}) |`;
  }).join("\n") + "\n";
}

export function manifestFor(release, automaticRelease = null) {
  if (!/^b\d+$/.test(release.tag_name) || release.draft || release.prerelease) throw new Error("Expected a stable redumper build tag.");
  const assets = {};
  for (const target of targets) {
    const name = `redumper-${release.tag_name}-${target}.zip`;
    const asset = release.assets.find(a => a.name === name && a.state === "uploaded" && a.size > 0);
    if (!asset || !/^sha256:[a-f0-9]{64}$/.test(asset.digest || "")) throw new Error(`Upstream asset or SHA-256 missing: ${name}`);
    assets[target] = { name, sha256: asset.digest.slice(7) };
  }
  return { tag: release.tag_name, automaticRelease, assets };
}

export function nextPatch(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
  if (!match) throw new Error("Automatic redumper updates require a stable app version.");
  return `${match[1]}.${match[2]}.${Number(match[3]) + 1}`;
}

export function setVersion(root, version, notes, manifest) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error("Invalid app version.");
  const read = file => fs.readFileSync(path.join(root, file), "utf8");
  const json = file => JSON.parse(read(file));
  const pkg = json("package.json"), lock = json("package-lock.json"), config = json("src-tauri/tauri.conf.json");
  const cargo = read("src-tauri/Cargo.toml"), cargoLock = read("src-tauri/Cargo.lock");
  const cargoVersion = /(\[package\][\s\S]*?\nversion = ")[^"]+("\n)/;
  const lockedVersion = /(\[\[package\]\]\nname = "tauri-app"\nversion = ")[^"]+("\n)/;
  if (!cargoVersion.test(cargo) || !lockedVersion.test(cargoLock)) throw new Error("Cannot locate application version in Cargo metadata.");
  pkg.version = lock.version = lock.packages[""].version = config.version = version;
  const pretty = value => JSON.stringify(value, null, 2) + "\n";
  const writes = {
    "package.json": pretty(pkg), "package-lock.json": pretty(lock), "src-tauri/tauri.conf.json": pretty(config),
    "src-tauri/Cargo.toml": cargo.replace(cargoVersion, (_, a, b) => a + version + b),
    "src-tauri/Cargo.lock": cargoLock.replace(lockedVersion, (_, a, b) => a + version + b),
    "RELEASE_NOTES.md": notes.trimEnd() + "\n\n---\n\n" + downloadTable(version),
    ".redumper/upstream.json": pretty(manifest),
  };
  fs.mkdirSync(path.join(root, ".redumper"), { recursive: true });
  for (const [file, content] of Object.entries(writes)) fs.writeFileSync(path.join(root, file), content);
}
