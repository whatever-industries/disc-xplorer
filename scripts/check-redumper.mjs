// Invoked only by the upstream-check workflow. Git pushes and publication are
// explicit workflow steps; this script updates the checkout's metadata only.
import fs from "node:fs";
import { execFileSync } from "node:child_process";
import { manifestFor, nextPatch, setVersion } from "./release-metadata.mjs";

const api = endpoint => JSON.parse(execFileSync("gh", ["api", endpoint], { encoding: "utf8" }));
const current = JSON.parse(fs.readFileSync(".redumper/upstream.json", "utf8"));
const version = JSON.parse(fs.readFileSync("package.json", "utf8")).version;
const latest = api("repos/superg/redumper/releases/latest");
// Validate the complete asset set before changing any files. Upstream publishes
// assets asynchronously; a later scheduled run will retry an incomplete release.
const manifest = manifestFor(latest);
let changed = false;
let releaseVersion = version;
if (Number(manifest.tag.slice(1)) > Number(current.tag.slice(1))) {
  releaseVersion = nextPatch(version);
  manifest.automaticRelease = releaseVersion;
  setVersion(process.cwd(), releaseVersion,
    `### Updated disc dumping engine\n\nWe've updated the bundled redumper to [${manifest.tag}](https://github.com/superg/redumper/releases/tag/${manifest.tag}) on all six platforms. This build brings the upstream dumping fixes and drive support into Disc Xplorer.`, manifest);
  changed = true;
}

let shouldRelease = changed;
if (!changed && current.automaticRelease === version) {
  // Retry a failed build using the same app version, instead of bumping forever
  // or considering a committed manifest proof of a successful publication.
  try {
    const release = api(`repos/${process.env.GITHUB_REPOSITORY}/releases/tags/v${version}`);
    shouldRelease = release.draft;
  } catch (e) {
    if (!String(e.stderr).includes("404")) throw e;
    shouldRelease = true;
  }
}
const output = `changed=${changed}\nrelease=${shouldRelease}\ntag=v${releaseVersion}\nupstream=${manifest.tag}\n`;
if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, output);
console.log(output);
