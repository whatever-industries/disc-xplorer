# Agent instructions — Disc Xplorer

Tauri v2 desktop app (Rust backend in `src-tauri/`, React/TypeScript frontend in `src/`)
for browsing and extracting disc images, floppy flux dumps, and filesystem images.

## Build & test

- Typecheck frontend: `npx tsc --noEmit`
- Backend tests: `cd src-tauri && cargo test --lib`
- Dev run: `npm run tauri dev` (GUI window; needs a desktop session)
- Rust target dir is redirected to `/private/tmp/disc-xplorer-target` (see `.cargo/config.toml`).

## Release checklist (upversion + push)

1. Bump the version in **all four** places (they must stay in sync):
   - `package.json` → `"version"`
   - `src-tauri/tauri.conf.json` → `"version"`
   - `src-tauri/Cargo.toml` → `version` (then run `cargo check` to refresh `Cargo.lock`)
   - `src/App.tsx` → statusbar `v…` string
2. Run the checks above; all must pass.
3. Update `RELEASE_NOTES.md` (user-facing highlights; keep the download-table format,
   artifact names are `Disc.Xplorer_<OS>_<arch>_vX.Y.Z.<ext>` — macOS ARM `.zip`,
   Windows `.exe`/`.msi`, Linux `.AppImage`).
4. Commit as `vX.Y.Z: <short summary>` on `main`, then tag `vX.Y.Z` and push both:
   `git push && git push origin vX.Y.Z`. The tag push triggers
   `.github/workflows/release.yml`, which builds all platforms and drafts the release.

## Hard rules

- **Never** add `Co-Authored-By`, "Generated with", or any AI/Claude attribution to commits.
- **Never** commit `logos/old/` or `logos/wip/`.
- Confirm with the user before overwriting existing files.
- Version bumps: don't forget the statusbar string in `App.tsx` (rule #1 exists because
  it was missed once).
