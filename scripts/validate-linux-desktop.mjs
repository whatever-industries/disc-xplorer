import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

// Inspect the extracted package, not just its source config: AppImage managers
// copy these names into shared desktop/icon directories (issue #17).
export function validateLinuxDesktop(root) {
  const desktopFiles = fs.readdirSync(root).filter(name => name.endsWith('.desktop'));
  if (desktopFiles.length !== 1) throw Error('Expected one AppImage desktop entry');
  const desktop = fs.readFileSync(path.join(root, desktopFiles[0]), 'utf8');
  const fields = Object.fromEntries(desktop.split(/\r?\n/).filter(line => /^[A-Za-z][^=]*=/.test(line))
    .map(line => { const index = line.indexOf('='); return [line.slice(0, index), line.slice(index + 1)]; }));
  if (fields.Icon !== 'disc-xplorer') throw Error('AppImage icon must use the unique disc-xplorer name');
  if (!/^disc-xplorer(?:\s|$)/.test(fields.Exec || '')) throw Error('Desktop launcher does not match the Linux binary');
  if (!fs.statSync(path.join(root, 'usr/bin/disc-xplorer')).isFile()) throw Error('Linux launcher binary missing');
  const dirIcon = fs.realpathSync(path.join(root, '.DirIcon'));
  const relative = path.relative(fs.realpathSync(root), dirIcon);
  if (relative.startsWith('..') || path.isAbsolute(relative) || !dirIcon.endsWith('.png')
      || path.basename(dirIcon) === 'tauri-app.png' || fs.statSync(dirIcon).size === 0) {
    throw Error('AppImage .DirIcon must resolve to a nonempty, uniquely named bundled PNG');
  }
  const theme = path.join(root, 'usr/share/icons/hicolor');
  const icons = fs.readdirSync(theme).map(size => path.join(theme, size, 'apps/disc-xplorer.png'))
    .filter(file => fs.existsSync(file) && fs.statSync(file).size > 0);
  if (!icons.length) throw Error('Unique desktop theme icon missing');
  if (!fields.Categories?.split(';').includes('Utility') || !fields.Comment) throw Error('Linux desktop metadata missing');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  validateLinuxDesktop(process.argv[2]);
  console.log('AppImage desktop launcher, unique icons and .DirIcon verified.');
}
