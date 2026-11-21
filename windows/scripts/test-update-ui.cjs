const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const html = fs.readFileSync(path.join(__dirname, '../codenotch/ui/settings.html'), 'utf8');
const start = html.indexOf('/* ---- updates ');
const end = html.indexOf('/* ---- start ', start);
assert.ok(start >= 0 && end > start);

const calls = [];
const button = { disabled: false, textContent: '', addEventListener(_event, callback) { this.click = callback; } };
const status = { textContent: '' };
const listeners = {};
const context = vm.createContext({
  document: { getElementById: id => id === 'btn-update' ? button : status },
  ui: text => text,
  invoke: command => { calls.push(command); return Promise.resolve({}); },
  strip: error => { throw new Error(error); },
  errText: error => String(error),
  window: { __TAURI__: { event: { listen: (name, callback) => { listeners[name] = callback; } } } },
});
vm.runInContext(html.slice(start, end), context);

(async () => {
  await new Promise(setImmediate);
  assert.equal(status.textContent, '', 'unchecked must not say up to date');
  assert.equal(button.textContent, 'Check for updates');

  listeners.update_state({ payload: { available: '1.20.0', checked: true, can_install: false } });
  assert.equal(status.textContent, '1.20.0');
  assert.equal(button.textContent, 'Download installer');
  button.click();
  assert.equal(calls.at(-1), 'open_update_installer');

  context.renderUpdate({ available: '1.20.0', checked: true, can_install: true });
  assert.equal(button.textContent, 'Update');
  button.click();
  assert.equal(calls.at(-1), 'install_update');

  context.renderUpdate({ checking: true });
  assert.equal(button.disabled, true);
  assert.equal(status.textContent, 'Checking…');

  context.renderUpdate({ message: 'Could not check for updates' });
  assert.equal(button.disabled, false);
  assert.equal(status.textContent, 'Could not check for updates');
  button.click();
  assert.equal(calls.at(-1), 'check_for_update');

  context.renderUpdate({ checked: true });
  assert.equal(status.textContent, 'Up to date');
  context.renderUpdate({ available: '1.20.0', checked: true, can_install: false, message: 'Could not install the update' });
  assert.equal(status.textContent, 'Could not install the update');
  assert.equal(button.textContent, 'Download installer');

  // Linux: no release carries an installer, so the row names neither Windows nor one.
  const linuxButton = { disabled: false, textContent: '', addEventListener() {} };
  const caption = { textContent: 'Codenotch checks for new Windows releases shortly after it starts.' };
  const linuxKeys = ['Codenotch checks for new releases shortly after it starts. There is no Linux installer yet.', 'Open release page'];
  const linux = vm.createContext({
    document: { getElementById: id => id === 'btn-update' ? linuxButton : id === 'cap-update' ? caption : { textContent: '' } },
    navigator: { platform: 'Linux x86_64' },
    LINUX_UPDATE_KEYS: linuxKeys,
    ui: text => text,
    invoke: () => Promise.resolve({}),
    window: { __TAURI__: { event: { listen() {} } } },
  });
  vm.runInContext(html.slice(start, end), linux);
  assert.equal(caption.textContent, linuxKeys[0]);
  linux.renderUpdate({ available: '1.20.0', checked: true, can_install: false });
  assert.equal(linuxButton.textContent, 'Open release page');
  // Every language that has the Windows wording has the Linux one too.
  const table = vm.runInNewContext(html.slice(html.indexOf('const LINUX_UPDATE_KEYS'), html.indexOf('for(const [lang, values] of Object.entries(LINUX_UPDATE_COPY))')) + ';({ keys: LINUX_UPDATE_KEYS, copy: LINUX_UPDATE_COPY })');
  const languages = html.match(/const STATIC_TEXT = \{([^}]*)\}/)[1].split(',').map(entry => entry.split(':')[0].trim().replace(/'/g, ''));
  assert.deepEqual(Object.keys(table.copy).sort(), languages.sort());
  for (const [lang, values] of Object.entries(table.copy)) {
    assert.equal(values.length, table.keys.length, lang);
    assert.ok(values.every(value => value && !/Windows/.test(value)), lang);
  }
  assert.ok(!/Windows|installer from/.test(table.keys[0]));
  console.log('PASS: unchecked, manual, signed, busy, error and current update states, and the Linux wording');
})().catch(error => { console.error(error); process.exitCode = 1; });
