# Windows file type selection

`template.nsi` is Tauri's NSIS template from `tauri-cli-v2.10.1`, commit
`9b17a7aeae9a83222ffe829aa4e2d8a5ba6bed8c`:
https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.10.1/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi

Upstream is MIT / Apache-2.0; both licenses are included here. The local changes
are limited to inserting the file type page and its functions, loading choices
before the previous uninstaller runs (and for silent installs), and replacing
Tauri's unconditional association/unassociation loops with our macros. Preserve
WebView2, upgrade, architecture and uninstall handling when refreshing the template.
The frontend regression suite checks the locked CLI version so an upgrade prompts
a template review.

`../installer-hooks.nsh` owns the three-column checkbox catalog. Keep its extensions
in sync with `bundle.fileAssociations` in `../tauri.conf.json`; a test enforces this.
Choices survive Back/Next and upgrades. New installs select all by default, matching
the old installer's default. Silent/passive installs use saved choices without a
prompt. Older installations infer choices from the associations they still own.

Each extension now has its own `DiscXplorer.<extension>` class. Reinstalling
preserves the original handler backup, and removal only restores it when Disc
Xplorer is still the registered handler. The old shared `Disc Image` class is
migrated only when its command belongs to the previous Disc Xplorer installation.
Windows' protected `UserChoice` defaults are never modified.

The Windows CI job compiles and runs `../../tests/installer-associations.nsi`
against an isolated HKCU registry namespace. It also compiles the checkbox page;
interactive layout, keyboard navigation and scaling still need a Windows check.
For manual testing, run the normal installer and test Select all / Clear all,
individual choices, Back/Next, an upgrade, and uninstall. Check 100%, 150% and 200%
Windows scaling. Do not infer Windows visual results from a macOS build.
