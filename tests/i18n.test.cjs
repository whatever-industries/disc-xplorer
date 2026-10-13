const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');
const root = path.join(__dirname, '../src');
const read = name => JSON.parse(fs.readFileSync(path.join(root, 'locales', `${name}.json`), 'utf8'));
const english = read('en');
const locales = ['es', 'fr', 'de', 'it', 'pl', 'ru', 'uk', 'pt-BR', 'zh-CN', 'ja', 'ko', 'hi', 'ar'];
const placeholders = text => (text.match(/\{\d+\}/g) || []).sort();
function harness(saved = null, system = ['en-US'], blocked = false) {
  const events = {}, storage = new Map(saved ? [['interfaceLanguage', saved]] : []);
  const source = ts.transpileModule(fs.readFileSync(path.join(root, 'i18n.ts'), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  const context = { exports: {}, require: name => require(path.join(root, name)), navigator: { languages: system },
    window: { addEventListener: (name, fn) => { events[name] = fn; } },
    localStorage: {
      getItem: key => { if (blocked) throw Error('blocked'); return storage.get(key); },
      setItem: (key, value) => { if (blocked) throw Error('blocked'); storage.set(key, value); },
    } };
  vm.runInNewContext(source, context);
  return { api: context.exports, events, storage, navigator: context.navigator };
}

test('every locale covers the English catalog and preserves interpolation and CLI tokens', () => {
  for (const locale of locales) {
    const catalog = read(locale);
    assert.deepEqual(Object.keys(catalog).sort(), Object.keys(english).sort(), locale);
    for (const [key, value] of Object.entries(catalog)) {
      assert.equal(typeof value, 'string');
      assert.ok(value.trim(), `${locale}: ${key}`);
      assert.deepEqual(placeholders(value), placeholders(key), `${locale}: ${key}`);
      for (const flag of key.match(/--[a-z][a-z-]+/g) || []) assert.ok(value.includes(flag), `${locale}: ${flag}`);
      assert.doesNotMatch(value, /ZXQ\d+QXZ/, `${locale}: ${key}`);
    }
  }
});

test('system locale detection, regional variants and fallback are deterministic', () => {
  const { resolveLanguage } = harness().api;
  assert.equal(resolveLanguage('system', ['es-MX']), 'es');
  assert.equal(resolveLanguage('system', ['pt-PT']), 'pt-BR');
  assert.equal(resolveLanguage('system', ['zh-Hans-SG']), 'zh-CN');
  assert.equal(resolveLanguage('system', ['zh-TW']), 'en');
  assert.equal(resolveLanguage('system', ['nl-NL', 'de-AT']), 'de');
  assert.equal(resolveLanguage('system', []), 'en');
  assert.equal(resolveLanguage('constructor', []), 'en');
  assert.equal(resolveLanguage('fr', ['de']), 'fr');
  assert.equal(harness('invalid', ['ja']).api.currentLanguage(), 'ja');
});

test('language switches persist without remounting and synchronize other windows', () => {
  const h = harness(); let changes = 0;
  const off = h.api.subscribeLanguage(() => changes++);
  h.api.setLanguage('fr');
  assert.equal(h.storage.get('interfaceLanguage'), 'fr');
  assert.equal(h.api.tr('Settings'), read('fr').Settings);
  h.events.storage({ key: 'interfaceLanguage', newValue: 'ja' });
  assert.equal(h.api.currentLanguage(), 'ja');
  assert.equal(changes, 2);
  h.events.storage({ key: 'theme', newValue: 'dark' });
  assert.equal(changes, 2);
  off(); h.api.setLanguage('de'); assert.equal(changes, 2);
});

test('system language updates and unavailable storage remain usable', () => {
  const h = harness(null, ['en-US'], true); let changes = 0;
  h.api.subscribeLanguage(() => changes++);
  h.navigator.languages = ['ar']; h.events.languagechange();
  assert.equal(h.api.currentLanguage(), 'ar'); assert.equal(changes, 1);
  h.api.setLanguage('ko'); assert.equal(h.api.currentLanguage(), 'ko');
});

test('interpolation preserves paths and filenames including dollar signs and braces', () => {
  const { translate, localizeMessage } = harness().api;
  const file = String.raw`C:\Images\日本語 $& {1}.iso`;
  assert.equal(translate('en', 'Image A — {0}', file), `Image A — ${file}`);
  assert.equal(translate('ar', 'Image A — {0}', file).includes(`\u2068${file}\u2069`), true);
  assert.equal(translate('de', 'Unknown diagnostic'), 'Unknown diagnostic');
  assert.equal(localizeMessage('*** DUMP\nSCSI: 0'), '*** DUMP\nSCSI: 0');
  assert.equal(localizeMessage(file), file);
});

test('known status templates and appended summaries translate without changing source data', () => {
  const h = harness('fr');
  assert.equal(h.api.localizeMessage('Image name cannot end with a space.'), read('fr')['Image name cannot end with a space.']);
  assert.equal(h.api.localizeMessage('Opening Game.iso…'), h.api.tr('Opening {0}…', 'Game.iso'));
  assert.equal(h.api.localizeMessage('Dump completed. Logs archived.'), `${h.api.tr('Dump completed.')} ${h.api.tr('Logs archived.')}`);
  assert.equal(h.api.localizeStage('Dump 2 · HASH'), h.api.tr('Dump {0} · {1}', '2', h.api.tr('Calculating hashes')));
});
