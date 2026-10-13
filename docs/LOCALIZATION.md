# Interface translations

Disc Xplorer bundles its translations; changing language never contacts a service.
Choose **Settings → General → Language**. System default follows the OS/browser
locale; unsupported locales fall back to English. The choice is saved locally and
shared with Sector View windows. Switching languages does not restart operations.

`src/locales/en.json` is the English catalog. Its English phrases are the stable
keys used by `tr()`; other JSON catalogs overlay their translated values.
Use numbered placeholders (`{0}`, `{1}`) for runtime values and whole sentences
instead of English plural suffixes. Preserve placeholders, CLI flags, file
extensions, and technical names exactly. `messages.json` lists recognized status
templates emitted in English by the backend. Unknown diagnostics remain verbatim.

Arabic uses right-to-left layout. Commands, paths, raw Redumper logs, filenames,
licenses, and hex bytes are not translated. Native OS dialog controls use the OS
language. Translations are an initial machine-assisted set with reviewed primary
controls; native-speaker wording and platform layout review are still welcome.

To add a locale, create its JSON catalog, import and register it in `src/i18n.ts`,
and add it to `tests/i18n.test.cjs`. Keep the selector's autonym readable to users
who accidentally select another language. Run `npm test` and `npm run build`.
Catalog tests check coverage, placeholders, command flags, language persistence,
window synchronization, fallback, and bidirectional interpolation.
