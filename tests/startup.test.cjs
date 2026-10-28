const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');
const ts = require('typescript');
const root = path.resolve(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'index.html'), 'utf8');
const bootstrap = html.match(/<script>([\s\S]*?)<\/script>/)[1];

test('startup applies only a valid saved theme before the application bundle', () => {
  assert.ok(html.indexOf('<style>') < html.indexOf('/src/main.tsx'));
  for (const theme of ['light', 'dark', 'system', null, 'invalid']) {
    const attributes = {};
    vm.runInNewContext(bootstrap, { localStorage: { getItem: () => theme },
      document: { documentElement: { setAttribute: (key, value) => { attributes[key] = value; } } } });
    assert.equal(attributes['data-theme'], ['light', 'dark'].includes(theme) ? theme : undefined);
  }
  assert.doesNotThrow(() => vm.runInNewContext(bootstrap, {
    localStorage: { getItem: () => { throw Error('storage unavailable'); } },
  }));
});

test('native windows start hidden and are shown once after the committed theme setup', async () => {
  const config = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri/tauri.conf.json')));
  const capability = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri/capabilities/default.json')));
  assert.equal(config.app.windows[0].visible, false);
  assert.ok(capability.permissions.includes('core:window:allow-show'));
  assert.ok(capability.permissions.includes('core:window:allow-set-theme'));
  const source = fs.readFileSync(path.join(root, 'src/App.tsx'), 'utf8');
  const ast = ts.createSourceFile('App.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let effect;
  function visit(node) {
    if (ts.isCallExpression(node) && node.expression.getText(ast) === 'useEffect'
        && node.arguments[0]?.getText(ast).includes('startupWindowShown')) effect = node.getText(ast);
    ts.forEachChild(node, visit);
  }
  visit(ast);
  assert.ok(effect);
  const js = ts.transpileModule(effect, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  for (const failTheme of [false, true]) {
    let resolveTheme, shows = 0;
    const context = {
      theme: 'dark', startupWindowShown: { current: false },
      localStorage: { setItem() {} }, document: { documentElement: { setAttribute() {}, removeAttribute() {} } },
      getCurrentWindow: () => ({ setTheme: () => new Promise((resolve, reject) => {
        resolveTheme = () => failTheme ? reject(Error('native theme unavailable')) : resolve();
      }), show: async () => { shows++; } }),
      useEffect: fn => fn(),
    };
    vm.createContext(context);
    vm.runInContext(js, context);
    assert.equal(shows, 0, 'do not expose the blank window while the native theme is pending');
    resolveTheme(); await new Promise(resolve => setImmediate(resolve));
    assert.equal(shows, 1);
    vm.runInContext(js, context); resolveTheme(); await new Promise(resolve => setImmediate(resolve));
    assert.equal(shows, 1, 'Strict Mode replay must not show the window twice');
  }
});
