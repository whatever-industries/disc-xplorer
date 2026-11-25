// Called by the scheduled bundled dependency workflow. This only prepares
// metadata. The workflow verifies and publishes an exact candidate commit.
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { manifestFor, nextPatch, setVersion } from "./release-metadata.mjs";

const encoderCrates = ["flac-bound", "libflac-sys", "mp3lame-encoder", "mp3lame-sys"];
const lockFile = "src-tauri/Cargo.lock";

export function encoderVersions(lock) {
  const versions = new Map();
  for (const item of lock.split(/^[ \t]*\[\[package\]\][ \t]*\r?$/m).slice(1)) {
    const name = /^name = "([^"]+)"\r?$/m.exec(item)?.[1];
    if (!encoderCrates.includes(name)) continue;
    const version = /^version = "(\d+\.\d+\.\d+)"\r?$/m.exec(item)?.[1];
    if (!version || versions.has(name)) throw new Error(`Expected one stable version of ${name} in Cargo.lock.`);
    versions.set(name, version);
  }
  for (const name of encoderCrates) {
    if (!versions.has(name)) throw new Error(`Missing ${name} from Cargo.lock.`);
  }
  return versions;
}

function compareVersion(before, after) {
  const old = before.split(".").map(Number), next = after.split(".").map(Number);
  return next[0] - old[0] || next[1] - old[1] || next[2] - old[2];
}

export function encoderChanges(oldLock, newLock) {
  const old = encoderVersions(oldLock), next = encoderVersions(newLock);
  const changed = [];
  for (const name of encoderCrates) {
    const comparison = compareVersion(old.get(name), next.get(name));
    if (comparison < 0) throw new Error(`Cargo tried to downgrade ${name}.`);
    if (comparison > 0) changed.push(name);
  }
  return changed;
}

function github(endpoint) {
  return JSON.parse(execFileSync("gh", ["api", endpoint], { encoding: "utf8" }));
}

function publicationPending(repository, version, api) {
  try {
    return api(`repos/${repository}/releases/tags/v${version}`).draft;
  } catch (error) {
    if (!String(error.stderr).includes("HTTP 404")) throw error;
    return true;
  }
}

function getRedumper(current, api) {
  const latest = api("repos/superg/redumper/releases/latest");
  if (!/^b\d+$/.test(latest.tag_name) || latest.draft || latest.prerelease) {
    throw new Error("Expected a stable redumper build tag.");
  }
  if (Number(latest.tag_name.slice(1)) <= Number(current.tag.slice(1))) return null;
  try {
    return manifestFor(latest);
  } catch (error) {
    // Release assets can arrive separately. Keep the current stable sidecar and
    // allow a compatible encoder update to proceed while awaiting the full set.
    if (!error.message.startsWith("Upstream asset or SHA-256 missing:")) throw error;
    console.warn(error.message);
    return null;
  }
}

function updateEncoders(root) {
  execFileSync("cargo", ["update", ...encoderCrates.flatMap(name => ["--package", name])], {
    cwd: path.join(root, "src-tauri"),
    stdio: "inherit",
  });
}

export function prepareUpdate(root, repository, api = github, update = updateEncoders) {
  const current = JSON.parse(fs.readFileSync(path.join(root, ".redumper/upstream.json"), "utf8"));
  const version = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).version;
  const result = { changed: false, release: false, tag: `v${version}`, redumper: false, flac: false, mp3: false };
  // An earlier automatic update may have merged after verification but failed
  // to upload or publish. Finish that release before reserving a new version.
  if (current.automaticRelease === version && publicationPending(repository, version, api)) {
    return { ...result, release: true };
  }

  const manifest = getRedumper(current, api);
  const lockPath = path.join(root, lockFile);
  const oldLock = fs.readFileSync(lockPath, "utf8");
  update(root);
  const changed = encoderChanges(oldLock, fs.readFileSync(lockPath, "utf8"));
  if (!changed.length) fs.writeFileSync(lockPath, oldLock);
  if (!manifest && !changed.length) return result;

  const next = nextPatch(version);
  const notes = [];
  if (manifest) notes.push(`Updated the bundled redumper to [${manifest.tag}](https://github.com/superg/redumper/releases/tag/${manifest.tag}).`);
  const flac = changed.some(name => name.includes("flac"));
  const mp3 = changed.some(name => name.includes("lame"));
  if (flac || mp3) {
    notes.push(`Updated the built-in ${flac && mp3 ? "FLAC and MP3 encoders" : flac ? "FLAC encoder" : "MP3 encoder"}.`);
  }
  setVersion(root, next, notes.length === 1 ? notes[0] : notes.map(note => `- ${note}`).join("\n"), {
    ...(manifest ?? current),
    automaticRelease: next,
  });
  return { changed: true, release: true, tag: `v${next}`, redumper: Boolean(manifest), flac, mp3 };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = prepareUpdate(process.cwd(), process.env.GITHUB_REPOSITORY);
  const output = Object.entries(result).map(([key, value]) => `${key}=${value}\n`).join("");
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, output);
  console.log(output);
}
