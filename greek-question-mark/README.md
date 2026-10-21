# Greek Question Mark or &#59;

A small browser quiz. U+037E GREEK QUESTION MARK and U+003B SEMICOLON look the
same in most fonts. Each round shows ten characters, one at a time, and you
guess which one each is.

Open `index.html` in a browser. It needs no server, build step, network or
dependencies, and runs fully offline.

## How to play

- **Bare** mode shows just the character.
- **In context** mode puts the character inside a line of JavaScript or C, a
  Greek sentence or an English sentence, boxed. The character is drawn
  independently of the snippet, so a Greek sentence says nothing about the
  answer.
- **Hard mode** forces a monospace font.
- **Debug** shows the code point under the glyph, for testing. Rounds played
  with debug on don't count toward your best score.

Each question is a fair 50/50 coin flip (`crypto.getRandomValues`). Rounds are
not balanced, so 7 of one and 3 of the other is normal. After each guess the
game shows the actual code point, Unicode name and UTF-8 bytes (`0xCD 0xBE` vs
`0x3B`). Best scores are kept per mode in `localStorage`. If storage is
blocked, the game still plays and just doesn't remember.

Keys: `1` or `←` for Greek Question Mark, `2` or `→` for Semicolon, `Enter`
for Next.

Copying the glyph into a hex inspector is allowed.

## Unicode handling

U+037E has a canonical decomposition to U+003B, so NFC, NFD, NFKC and NFKD
all turn the Greek question mark into a plain semicolon. An editor, formatter,
linter or git hook that normalizes text would silently ruin every question.
To guard against this:

- Neither character is written literally as game text in any file. Both are
  built at runtime with `String.fromCodePoint(0x037E)` and
  `String.fromCodePoint(0x003B)`. Snippet templates in `snippets.js` mark the
  spot with the `{{T}}` token. Even the semicolon in the page title is the
  character reference `&#59;`.
- Game text is never passed to `.normalize()` and only reaches the DOM through
  `textContent` and text nodes, never `innerHTML`.
- On load, a self-check writes both characters into a test element and reads
  back `codePointAt(0)`. It also checks that the two strings differ and that
  UTF-8 encoding gives the expected bytes. Each glyph is re-checked after it
  renders. Any mismatch shows a red error banner.
- The glyph uses the system UI font stack, where the two look the same (hard
  mode uses the system monospace stack).

## Accessibility

- All controls are real `<button>`s and form inputs, with a visible focus ring.
- Feedback is announced through an `aria-live` region and shown with text and
  ✓/✗, not color alone.
- Screen readers give the two characters different names, which would spoil
  the answer. The glyph region is `aria-hidden` and the question announces
  only "Character shown" (or "Character shown in a Greek sentence", etc.).

## Files

| File | What |
|------|------|
| `index.html` | Markup for the start, question and end screens |
| `style.css` | Layout, light/dark theme, font stacks |
| `game.js` | Pure game logic (exported for Node) and the UI |
| `snippets.js` | Context-mode templates using the `{{T}}` placeholder |
| `test/unicode.test.js` | Node tests, no dependencies |

## Tests

```sh
node greek-question-mark/test/unicode.test.js
```

The tests check:

- No source file contains a literal U+037E anywhere.
- No snippet template (any string literal holding the token, in any `.js`
  file) contains U+003B or U+037E, literally or as an escape such as `;`
  or `&#59;`.
- Every template has exactly one token.
- `game.js` never calls `.normalize()` or `innerHTML`.
- The generator produces both code points with the right characters, names
  and UTF-8 bytes, and every context snippet holds exactly one target.
- A round is 10 questions.
- Draws come out about 50/50 and rounds are not forced to 5/5.
