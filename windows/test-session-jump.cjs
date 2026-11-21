// Run with node --test test-session-jump.cjs from windows/.
// A session row in the Claude card names its session, and a click on it finds that session again.
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');

const html = readFileSync(join(__dirname, 'codenotch/ui/notch.html'), 'utf8');
const begin = '// BEGIN TESTABLE SESSION-JUMP SELECTOR';
const end = '// END TESTABLE SESSION-JUMP SELECTOR';
assert.equal(html.split(begin).length, 2, 'exactly one begin marker');
assert.equal(html.split(end).length, 2, 'exactly one end marker');
const context = vm.createContext({ decodeURIComponent, encodeURIComponent });
vm.runInContext(html.slice(html.indexOf(begin) + begin.length, html.indexOf(end)), context);

// What the browser makes of the row: the attribute's value, and closest() from anything inside it
function rowOf(id) {
  const attrs = context.sessionAttrs(id);
  const value = attrs.match(/data-session="([^"]*)"/);
  assert.ok(value, 'the row carries its session');
  const row = { dataset: { session: value[1] }, closest: sel => (sel === '.s-jump' ? row : null) };
  return { attrs, row, dot: { closest: sel => row.closest(sel) } };
}

test('a click on a session row, or on the dot inside it, names that session', () => {
  const { row, dot } = rowOf('3f2a-11');
  assert.equal(context.sessionAt(row), '3f2a-11');
  assert.equal(context.sessionAt(dot), '3f2a-11');
});
test('an id cannot break out of the attribute it is written into', () => {
  const id = 'a" onclick="x<y>&\'é';
  const { attrs, row } = rowOf(id);
  assert.equal(attrs.split('"').length, 5, 'two attributes, nothing more');
  assert.equal(context.sessionAt(row), id);
});
test('a click anywhere else in the card names no session', () => {
  assert.equal(context.sessionAt({ closest: () => null }), '');
  assert.equal(context.sessionAt(null), '');
  assert.equal(context.sessionAt({}), '');
});
test('a row with a mangled id is ignored rather than thrown on', () => {
  const row = { dataset: { session: '%E0%A4%A' }, closest: () => row };
  assert.equal(context.sessionAt(row), '');
});
test('the card listens for the click and asks for the jump', () => {
  assert.match(html, /card\.addEventListener\('click'[\s\S]{0,200}invoke\('focus_session',\{id\}\)/);
});
