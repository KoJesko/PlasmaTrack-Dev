// Tests for "Greek Question Mark or ;". No dependencies:
//   node greek-question-mark/test/unicode.test.js
//
// The two target characters are written here only as escapes, never
// literally, for the same reason as everywhere else in the game.

'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.join(__dirname, '..');
const GREEK = 0x037e;
const SEMI = 0x003b;
const GREEK_CHAR = String.fromCodePoint(GREEK);
const SEMI_CHAR = String.fromCodePoint(SEMI);

const SOURCE_EXT = new Set(['.html', '.css', '.js', '.md']);

function sourceFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name.startsWith('.') || entry.name === 'node_modules') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...sourceFiles(full));
    else if (SOURCE_EXT.has(path.extname(entry.name))) out.push(full);
  }
  return out;
}

function rel(file) {
  return path.relative(ROOT, file);
}

function lineOf(text, index) {
  return text.slice(0, index).split('\n').length;
}

// Yields every quoted string literal in JS source with its raw (unescaped)
// body and offset. Skips // and /* */ comments. Good enough for this
// project's sources, which use no regex literals containing quotes.
function* stringLiterals(src) {
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '/' && n === '/') {
      const end = src.indexOf('\n', i);
      i = end === -1 ? src.length : end;
    } else if (c === '/' && n === '*') {
      const end = src.indexOf('*/', i + 2);
      i = end === -1 ? src.length : end + 2;
    } else if (c === '"' || c === "'" || c === '`') {
      const start = i;
      i++;
      while (i < src.length && src[i] !== c) {
        if (src[i] === '\\') i++;
        i++;
      }
      yield { raw: src.slice(start + 1, i), index: start };
      i++;
    } else {
      i++;
    }
  }
}

// Escape spellings of either character that could sneak into a template.
const ESCAPED_TARGET =
  /\\u0*3[bB](?![0-9a-fA-F])|\\u\{0*3[bB]\}|\\x3[bB]|\\u037[eE]|\\u\{0*37[eE]\}|&#0*59;|&#[xX]0*3[bB];|&#0*894;|&#[xX]0*37[eE];|&semi;/;

const snippets = require('../snippets.js');
const game = require('../game.js');

test('no source file contains a literal U+037E anywhere', () => {
  const files = sourceFiles(ROOT);
  assert.ok(files.length >= 5, 'expected to find the game sources');
  for (const file of files) {
    const text = fs.readFileSync(file, 'utf8');
    const at = text.indexOf(GREEK_CHAR);
    assert.equal(at, -1, `${rel(file)}:${lineOf(text, at)} contains a literal U+037E`);
  }
});

test('no snippet template in any source file contains a literal target character', () => {
  const token = snippets.TOKEN;
  let templatesSeen = 0;
  for (const file of sourceFiles(ROOT).filter((f) => f.endsWith('.js'))) {
    if (file === __filename) continue;
    const src = fs.readFileSync(file, 'utf8');
    for (const lit of stringLiterals(src)) {
      if (lit.raw === token || !lit.raw.includes(token)) continue;
      templatesSeen++;
      const where = `${rel(file)}:${lineOf(src, lit.index)}`;
      assert.ok(!lit.raw.includes(SEMI_CHAR), `${where}: template contains a literal U+003B`);
      assert.ok(!lit.raw.includes(GREEK_CHAR), `${where}: template contains a literal U+037E`);
      assert.ok(!ESCAPED_TARGET.test(lit.raw), `${where}: template contains an escaped target character`);
    }
  }
  const total = snippets.categories.reduce((n, c) => n + c.templates.length, 0);
  assert.equal(templatesSeen, total, 'every loaded template should be found by the source scan');
});

test('snippets.js string literals hold no semicolon at all', () => {
  // snippets.js is pure data: a semicolon in any of its strings is a bug.
  const file = path.join(ROOT, 'snippets.js');
  const src = fs.readFileSync(file, 'utf8');
  for (const lit of stringLiterals(src)) {
    assert.ok(!lit.raw.includes(SEMI_CHAR), `snippets.js:${lineOf(src, lit.index)} has a semicolon in a string`);
  }
});

test('loaded templates have exactly one token and no target character', () => {
  const ids = snippets.categories.map((c) => c.id).sort();
  assert.deepEqual(ids, ['code', 'english', 'greek']);
  for (const cat of snippets.categories) {
    assert.ok(cat.templates.length >= 3, `${cat.id} needs a few templates`);
    for (const t of cat.templates) {
      assert.equal(t.split(snippets.TOKEN).length - 1, 1, `exactly one token in ${JSON.stringify(t)}`);
      assert.ok(!t.includes(SEMI_CHAR) && !t.includes(GREEK_CHAR), `target character in ${JSON.stringify(t)}`);
    }
  }
});

test('game code never normalizes text or builds HTML from strings', () => {
  for (const name of ['game.js', 'snippets.js']) {
    const src = fs.readFileSync(path.join(ROOT, name), 'utf8');
    assert.ok(!/\.normalize\s*\(/.test(src), `${name} calls .normalize()`);
    assert.ok(!/\b(innerHTML|outerHTML|insertAdjacentHTML)\b/.test(src), `${name} builds HTML from strings`);
    assert.ok(!/document\.write/.test(src), `${name} uses document.write`);
  }
});

test('character generator produces the exact code points', () => {
  assert.equal(game.charFor(GREEK).codePointAt(0), GREEK);
  assert.equal(game.charFor(SEMI).codePointAt(0), SEMI);
  assert.equal(game.charFor(GREEK).length, 1);
  assert.notEqual(game.charFor(GREEK), game.charFor(SEMI));
  // Documents the trap the game avoids.
  assert.equal(game.charFor(GREEK).normalize('NFC'), SEMI_CHAR);
});

test('facts shown after a guess are right', () => {
  assert.deepEqual(game.describe(GREEK), {
    cp: GREEK,
    codePoint: 'U+037E',
    name: 'GREEK QUESTION MARK',
    label: 'Greek Question Mark',
    utf8: '0xCD 0xBE'
  });
  assert.deepEqual(game.describe(SEMI), {
    cp: SEMI,
    codePoint: 'U+003B',
    name: 'SEMICOLON',
    label: 'Semicolon',
    utf8: '0x3B'
  });
});

// Deterministic RNG for repeatable tests (mulberry32).
function seeded(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return { bit: () => (next() < 0.5 ? 1 : 0), int: (n) => Math.floor(next() * n) };
}

test('a round is 10 questions in both modes', () => {
  assert.equal(game.ROUND_LENGTH, 10);
  for (const mode of ['bare', 'context']) {
    assert.equal(game.makeRound(mode).length, 10);
    assert.equal(game.makeRound(mode, seeded(1)).length, 10);
  }
});

test('generated questions carry both code points, matching their characters', () => {
  const seen = new Set();
  for (const mode of ['bare', 'context']) {
    for (let r = 0; r < 50; r++) {
      for (const q of game.makeRound(mode)) {
        assert.ok(q.cp === GREEK || q.cp === SEMI);
        assert.equal(q.char.codePointAt(0), q.cp);
        assert.equal(q.char.length, 1);
        seen.add(q.cp);
        if (mode === 'context') {
          assert.ok(q.snippet, 'context questions have a snippet');
          const text = q.snippet.before + q.char + q.snippet.after;
          const hits = [...text].filter((ch) => ch === SEMI_CHAR || ch === GREEK_CHAR);
          assert.equal(hits.length, 1, `exactly one target in ${JSON.stringify(text)}`);
        } else {
          assert.equal(q.snippet, null);
        }
      }
    }
  }
  assert.deepEqual([...seen].sort(), [SEMI, GREEK]);
});

test('draws are a fair coin and rounds are not forced to 5/5', () => {
  const rand = seeded(42);
  let greek = 0;
  let unbalanced = 0;
  const rounds = 2000;
  for (let r = 0; r < rounds; r++) {
    const n = game.makeRound('bare', rand).filter((q) => q.cp === GREEK).length;
    greek += n;
    if (n !== 5) unbalanced++;
  }
  const share = greek / (rounds * 10);
  assert.ok(share > 0.48 && share < 0.52, `Greek share ${share} is not about 0.5`);
  // With a fair coin only ~24.6% of rounds land on exactly 5/5.
  assert.ok(unbalanced > rounds * 0.6, 'rounds look balanced to 5/5');

  const real = game.cryptoRandom();
  let ones = 0;
  for (let i = 0; i < 20000; i++) ones += real.bit();
  assert.ok(ones > 9500 && ones < 10500, `crypto bit gave ${ones}/20000`);
  for (let i = 0; i < 1000; i++) {
    const v = real.int(7);
    assert.ok(Number.isInteger(v) && v >= 0 && v < 7);
  }
});
