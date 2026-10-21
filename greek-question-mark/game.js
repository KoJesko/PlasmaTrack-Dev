// "Greek Question Mark or ;" - game logic and UI.
//
// Unicode rules this file follows (see README.md):
// - Neither target character is written literally anywhere. Both are made
//   at runtime from their code points with String.fromCodePoint.
// - Game text is never normalized: U+037E canonically decomposes to U+003B,
//   so NFC/NFKC would silently turn every question into a semicolon.
// - Text reaches the DOM only through textContent / text nodes.
//
// The top half is pure logic and is exported for the Node tests. The UI
// only boots when a document exists.

(function (root) {
  'use strict';

  var ROUND_LENGTH = 10;

  var GREEK = 0x037e;
  var SEMI = 0x003b;

  var CHARS = {};
  CHARS[GREEK] = {
    cp: GREEK,
    key: 'greek',
    name: 'GREEK QUESTION MARK',
    label: 'Greek Question Mark'
  };
  CHARS[SEMI] = {
    cp: SEMI,
    key: 'semi',
    name: 'SEMICOLON',
    label: 'Semicolon'
  };

  var MODES = {
    bare: 'Bare',
    context: 'In context'
  };

  var SNIPPETS =
    typeof module !== 'undefined' && module.exports
      ? require('./snippets.js')
      : root.GQM_SNIPPETS;

  function charFor(cp) {
    return String.fromCodePoint(cp);
  }

  function formatCodePoint(cp) {
    var hex = cp.toString(16).toUpperCase();
    while (hex.length < 4) hex = '0' + hex;
    return 'U+' + hex;
  }

  function utf8Bytes(cp) {
    if (typeof TextEncoder !== 'undefined') {
      return Array.prototype.slice.call(new TextEncoder().encode(charFor(cp)));
    }
    // Only BMP code points are ever used here.
    if (cp < 0x80) return [cp];
    if (cp < 0x800) return [0xc0 | (cp >> 6), 0x80 | (cp & 0x3f)];
    return [0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f)];
  }

  function formatBytes(bytes) {
    return bytes
      .map(function (b) {
        var hex = b.toString(16).toUpperCase();
        return '0x' + (hex.length < 2 ? '0' + hex : hex);
      })
      .join(' ');
  }

  function describe(cp) {
    return {
      cp: cp,
      codePoint: formatCodePoint(cp),
      name: CHARS[cp].name,
      label: CHARS[cp].label,
      utf8: formatBytes(utf8Bytes(cp))
    };
  }

  // Randomness: crypto when available. bit() is an exact 50/50 and int(n)
  // uses rejection sampling, so neither is biased.
  function cryptoRandom() {
    var c = root.crypto || (typeof globalThis !== 'undefined' && globalThis.crypto);
    if (!c || !c.getRandomValues) {
      return {
        bit: function () {
          return Math.random() < 0.5 ? 1 : 0;
        },
        int: function (n) {
          return Math.floor(Math.random() * n);
        }
      };
    }
    var buf = new Uint32Array(1);
    function u32() {
      c.getRandomValues(buf);
      return buf[0];
    }
    return {
      bit: function () {
        return u32() & 1;
      },
      int: function (n) {
        var limit = 0x100000000 - (0x100000000 % n);
        var x;
        do {
          x = u32();
        } while (x >= limit);
        return x % n;
      }
    };
  }

  function splitTemplate(template) {
    var at = template.indexOf(SNIPPETS.TOKEN);
    return {
      before: template.slice(0, at),
      after: template.slice(at + SNIPPETS.TOKEN.length)
    };
  }

  // Every question is an independent coin flip. Rounds are deliberately not
  // balanced to 5/5, and the snippet is picked independently of the answer.
  function makeRound(mode, rand) {
    rand = rand || cryptoRandom();
    var used = {};
    var questions = [];
    for (var i = 0; i < ROUND_LENGTH; i++) {
      var cp = rand.bit() ? GREEK : SEMI;
      var q = { index: i, cp: cp, char: charFor(cp), snippet: null, guess: null, correct: null };
      if (mode === 'context') {
        var cats = SNIPPETS.categories;
        var cat = cats[rand.int(cats.length)];
        var fresh = cat.templates.filter(function (t) {
          return !used[t];
        });
        var pool = fresh.length ? fresh : cat.templates;
        var template = pool[rand.int(pool.length)];
        used[template] = true;
        var parts = splitTemplate(template);
        q.snippet = { category: cat.id, label: cat.label, before: parts.before, after: parts.after };
      }
      questions.push(q);
    }
    return questions;
  }

  var core = {
    ROUND_LENGTH: ROUND_LENGTH,
    GREEK: GREEK,
    SEMI: SEMI,
    CHARS: CHARS,
    MODES: MODES,
    charFor: charFor,
    formatCodePoint: formatCodePoint,
    utf8Bytes: utf8Bytes,
    formatBytes: formatBytes,
    describe: describe,
    cryptoRandom: cryptoRandom,
    splitTemplate: splitTemplate,
    makeRound: makeRound
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = core;
  }
  if (typeof document === 'undefined') return;

  // ---------------------------------------------------------------- UI ----

  var STORAGE_PREFIX = 'gqm.';

  function store(key, value) {
    try {
      root.localStorage.setItem(STORAGE_PREFIX + key, String(value));
    } catch (e) {
      // Private mode or blocked storage: the game still works, it just forgets.
    }
  }

  function load(key) {
    try {
      return root.localStorage.getItem(STORAGE_PREFIX + key);
    } catch (e) {
      return null;
    }
  }

  function bestScore(mode) {
    var raw = load('best.' + mode);
    var n = raw === null ? NaN : parseInt(raw, 10);
    return n >= 0 && n <= ROUND_LENGTH ? n : null;
  }

  function $(id) {
    return document.getElementById(id);
  }

  function clear(el) {
    while (el.firstChild) el.removeChild(el.firstChild);
  }

  function el(tag, className, text) {
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  // Builds "before [target] after" from text nodes and a boxed span.
  function renderSnippet(container, q) {
    container.appendChild(document.createTextNode(q.snippet.before));
    var target = el('span', 'target', q.char);
    container.appendChild(target);
    container.appendChild(document.createTextNode(q.snippet.after));
    return target;
  }

  var ui = {};
  var state = {
    screen: 'start',
    mode: 'bare',
    questions: [],
    current: 0,
    answered: false,
    debugUsed: false
  };

  function showError(message) {
    ui.errorText.textContent = message;
    ui.errorBanner.hidden = false;
  }

  // The DOM must hold exactly the code point we asked for. A tool that
  // normalized the page or a broken text pipeline would show up here.
  function checkRendered(node, cp) {
    var text = node.textContent;
    var got = text.codePointAt(0);
    if (text.length !== 1 || got !== cp) {
      return (
        'expected ' + formatCodePoint(cp) + ' but the page holds ' +
        (got === undefined ? 'nothing' : formatCodePoint(got)) +
        (text.length > 1 ? ' (+' + (text.length - 1) + ' more)' : '')
      );
    }
    return null;
  }

  function selfCheck() {
    var problems = [];
    if (charFor(GREEK) === charFor(SEMI)) {
      problems.push('U+037E and U+003B came out as the same string');
    }
    [GREEK, SEMI].forEach(function (cp) {
      ui.selfcheck.textContent = charFor(cp);
      var problem = checkRendered(ui.selfcheck, cp);
      if (problem) problems.push(problem);
    });
    clear(ui.selfcheck);
    var greekBytes = formatBytes(utf8Bytes(GREEK));
    var semiBytes = formatBytes(utf8Bytes(SEMI));
    if (greekBytes !== '0xCD 0xBE' || semiBytes !== '0x3B') {
      problems.push('UTF-8 encoding gave ' + greekBytes + ' / ' + semiBytes);
    }
    if (problems.length) {
      showError('Unicode self-check failed: ' + problems.join('. ') + '. Answers may be wrong.');
    }
  }

  function setScreen(name) {
    state.screen = name;
    ui.startScreen.hidden = name !== 'start';
    ui.playScreen.hidden = name !== 'play';
    ui.endScreen.hidden = name !== 'end';
  }

  function selectedMode() {
    var checked = document.querySelector('input[name="mode"]:checked');
    return checked && MODES[checked.value] ? checked.value : 'bare';
  }

  function updateBest() {
    var mode = selectedMode();
    var best = bestScore(mode);
    ui.bestScore.textContent =
      'Best (' + MODES[mode] + '): ' + (best === null ? 'none yet' : best + '/' + ROUND_LENGTH);
  }

  function applyToggles() {
    document.body.classList.toggle('hard', ui.hardToggle.checked);
    document.body.classList.toggle('debug', ui.debugToggle.checked);
    store('hard', ui.hardToggle.checked ? 1 : 0);
    store('debug', ui.debugToggle.checked ? 1 : 0);
  }

  function startRound() {
    state.mode = selectedMode();
    store('mode', state.mode);
    state.questions = makeRound(state.mode);
    state.current = 0;
    state.debugUsed = ui.debugToggle.checked;
    ui.playMode.textContent = MODES[state.mode] + (ui.hardToggle.checked ? ' · hard' : '');
    setScreen('play');
    showQuestion();
  }

  function score() {
    return state.questions.filter(function (q) {
      return q.correct;
    }).length;
  }

  function showQuestion() {
    var q = state.questions[state.current];
    state.answered = false;

    ui.progressText.textContent = 'Question ' + (state.current + 1) + '/' + ROUND_LENGTH;
    ui.runningScore.textContent = 'Score ' + score();

    clear(ui.stage);
    var target;
    if (q.snippet) {
      ui.stage.className = 'stage stage-context';
      var line = el('p', 'snippet');
      line.dir = 'ltr';
      target = renderSnippet(line, q);
      ui.stage.appendChild(line);
    } else {
      ui.stage.className = 'stage stage-bare';
      target = el('span', 'glyph', q.char);
      ui.stage.appendChild(target);
    }

    var problem = checkRendered(target, q.cp);
    if (problem) showError('Rendered glyph check failed on question ' + (q.index + 1) + ': ' + problem + '.');

    ui.debugCp.textContent = 'Debug: ' + formatCodePoint(q.cp) + ' ' + CHARS[q.cp].name;
    ui.stageLabel.textContent = q.snippet ? 'Character shown in ' + q.snippet.label + '.' : 'Character shown.';

    ui.answerGreek.disabled = false;
    ui.answerSemi.disabled = false;
    ui.answerGreek.classList.remove('chosen');
    ui.answerSemi.classList.remove('chosen');
    ui.feedback.hidden = true;
    ui.feedbackLive.textContent = '';
    ui.nextBtn.hidden = true;

    ui.progress.focus();
  }

  function answer(cp) {
    if (state.screen !== 'play' || state.answered) return;
    var q = state.questions[state.current];
    state.answered = true;
    q.guess = cp;
    q.correct = cp === q.cp;

    ui.answerGreek.disabled = true;
    ui.answerSemi.disabled = true;
    (cp === GREEK ? ui.answerGreek : ui.answerSemi).classList.add('chosen');
    ui.runningScore.textContent = 'Score ' + score();

    var d = describe(q.cp);
    ui.feedback.hidden = false;
    ui.feedback.className = 'feedback ' + (q.correct ? 'is-correct' : 'is-wrong');
    ui.verdictIcon.textContent = q.correct ? '✓' : '✗';
    ui.verdictText.textContent = q.correct ? 'Correct' : 'Wrong';
    ui.factCp.textContent = d.codePoint;
    ui.factName.textContent = d.name;
    ui.factBytes.textContent = d.utf8;
    ui.factGuess.textContent = CHARS[cp].label;

    ui.feedbackLive.textContent =
      (q.correct ? 'Correct. ' : 'Wrong. ') +
      'You said ' + CHARS[cp].label + '. It was ' + d.codePoint + ', ' + d.name + ', UTF-8 bytes ' + d.utf8 + '.';

    var last = state.current === ROUND_LENGTH - 1;
    ui.nextBtn.textContent = last ? 'See results' : 'Next';
    ui.nextBtn.hidden = false;
    ui.nextBtn.focus();
  }

  function next() {
    if (state.screen !== 'play' || !state.answered) return;
    if (state.current < ROUND_LENGTH - 1) {
      state.current++;
      showQuestion();
    } else {
      showEnd();
    }
  }

  function showEnd() {
    var total = score();
    var prev = bestScore(state.mode);
    var isBest = !state.debugUsed && (prev === null || total > prev);
    if (isBest) store('best.' + state.mode, total);

    ui.finalScore.textContent = total + '/' + ROUND_LENGTH;
    var best = bestScore(state.mode);
    ui.finalBest.textContent = state.debugUsed
      ? 'Debug was on, so this round does not count toward your best.'
      : isBest
        ? 'New best for ' + MODES[state.mode] + '!'
        : 'Best for ' + MODES[state.mode] + ': ' + best + '/' + ROUND_LENGTH + '.';

    clear(ui.breakdown);
    state.questions.forEach(function (q) {
      var li = el('li', q.correct ? 'is-correct' : 'is-wrong');
      var head = el('div', 'bd-head');
      head.appendChild(el('span', 'bd-icon', q.correct ? '✓' : '✗'));
      head.appendChild(el('span', 'bd-num', 'Q' + (q.index + 1)));
      head.appendChild(el('span', 'bd-verdict', q.correct ? 'Correct' : 'Wrong'));
      li.appendChild(head);

      var d = describe(q.cp);
      li.appendChild(
        el('p', 'bd-detail', 'Shown: ' + d.codePoint + ' ' + d.name + ' (' + d.utf8 + ')' +
          ' · You said: ' + CHARS[q.guess].label)
      );
      if (q.snippet) {
        var snip = el('p', 'snippet bd-snippet');
        snip.dir = 'ltr';
        snip.setAttribute('aria-hidden', 'true');
        renderSnippet(snip, q);
        li.appendChild(snip);
      }
      ui.breakdown.appendChild(li);
    });

    setScreen('end');
    updateBest();
    ui.endHeading.focus();
  }

  function onKey(e) {
    if (e.altKey || e.ctrlKey || e.metaKey || e.repeat) return;
    if (state.screen !== 'play') return;
    if (!state.answered) {
      if (e.key === '1' || e.key === 'ArrowLeft') {
        e.preventDefault();
        answer(GREEK);
      } else if (e.key === '2' || e.key === 'ArrowRight') {
        e.preventDefault();
        answer(SEMI);
      }
    } else if (e.key === 'Enter') {
      // Also stops a focused button's own Enter click from firing twice.
      e.preventDefault();
      next();
    }
  }

  function init() {
    [
      'errorBanner', 'errorText', 'selfcheck',
      'startScreen', 'playScreen', 'endScreen',
      'bestScore', 'hardToggle', 'debugToggle', 'playBtn',
      'progress', 'progressText', 'runningScore', 'playMode', 'stage', 'stageLabel', 'debugCp',
      'answerGreek', 'answerSemi', 'feedback', 'feedbackLive',
      'verdictIcon', 'verdictText', 'factCp', 'factName', 'factBytes', 'factGuess', 'nextBtn',
      'endHeading', 'finalScore', 'finalBest', 'breakdown', 'againBtn', 'menuBtn'
    ].forEach(function (id) {
      ui[id] = $(id);
    });

    var savedMode = load('mode');
    if (savedMode && MODES[savedMode]) {
      var radio = document.querySelector('input[name="mode"][value="' + savedMode + '"]');
      if (radio) radio.checked = true;
    }
    ui.hardToggle.checked = load('hard') === '1';
    ui.debugToggle.checked = load('debug') === '1';
    applyToggles();
    updateBest();

    document.querySelectorAll('input[name="mode"]').forEach(function (r) {
      r.addEventListener('change', updateBest);
    });
    ui.hardToggle.addEventListener('change', applyToggles);
    ui.debugToggle.addEventListener('change', applyToggles);
    ui.playBtn.addEventListener('click', startRound);
    ui.answerGreek.addEventListener('click', function () {
      answer(GREEK);
    });
    ui.answerSemi.addEventListener('click', function () {
      answer(SEMI);
    });
    ui.nextBtn.addEventListener('click', next);
    ui.againBtn.addEventListener('click', startRound);
    ui.menuBtn.addEventListener('click', function () {
      setScreen('start');
      updateBest();
      ui.playBtn.focus();
    });
    document.addEventListener('keydown', onKey);

    selfCheck();
    setScreen('start');
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})(this);
