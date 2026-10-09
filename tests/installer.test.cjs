const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const root = path.resolve(__dirname, '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8').replace(/\r\n/g, '\n');
const config = JSON.parse(read('src-tauri/tauri.conf.json'));
const hooks = read('src-tauri/installer-hooks.nsh');
const catalog = [...hooks.matchAll(/^  !insertmacro \$\{CALLBACK\} (\w+)\s+"([^"]+)"\s+(\d+)u\s+(\d+)u$/gm)];

test('Windows checkbox catalog covers every configured association exactly once', () => {
  const types = catalog.map(m => m[1]);
  assert.equal(new Set(types).size, types.length);
  assert.deepEqual(types.sort(), config.bundle.fileAssociations.flatMap(a => a.ext).sort());
  // Standard MUI content area is 300 x 140 dialog units; no checkbox overlaps.
  const occupied = new Set();
  for (const [, , label, x, y] of catalog) {
    assert.ok(Number(x) + 96 <= 300 && Number(y) + 10 <= 140, label);
    assert.ok(!occupied.has(`${x},${y}`), label);
    occupied.add(`${x},${y}`);
  }
});

test('Windows installer uses selective registration and a reviewed Tauri template', () => {
  const version = JSON.parse(read('package-lock.json')).packages['node_modules/@tauri-apps/cli'].version;
  assert.equal(version, '2.10.1', 'Review and refresh the custom NSIS template when upgrading Tauri CLI');
  const template = read(`src-tauri/${config.bundle.windows.nsis.template}`);
  assert.equal(config.bundle.windows.nsis.installerHooks, 'installer-hooks.nsh');
  assert.ok(template.indexOf('Page custom DX_AssociationPage') < template.indexOf('!insertmacro MUI_PAGE_INSTFILES'));
  assert.equal((template.match(/!insertmacro DX_INSTALL_ASSOCIATIONS/g) || []).length, 1);
  assert.equal((template.match(/!insertmacro DX_UNINSTALL_ASSOCIATIONS/g) || []).length, 1);
  assert.ok(!/!insertmacro APP_(?:UN)?ASSOCIATE /.test(template), 'Unconditional Tauri association loops must stay removed');
  assert.ok(template.includes('Function PageReinstall\n  Call DX_InitAssociations'));
  assert.ok(template.includes('Section EarlyChecks\n  ; Silent installs do not visit pages.\n  Call DX_InitAssociations'));
});
