// Context-mode snippets for "Greek Question Mark or ;".
//
// Each template holds exactly one TOKEN where the target character goes.
// The target character itself must NEVER appear in this file, neither
// U+037E nor U+003B, not even inside the surrounding text: it is generated
// at runtime with String.fromCodePoint (see game.js). Templates also avoid
// any other semicolon, so the boxed character is the only candidate.
// test/unicode.test.js enforces all of this.
//
// Which character is shown is drawn independently of the snippet, so a
// Greek sentence is no hint that the answer is the Greek question mark.

(function (root) {
  'use strict';

  var SNIPPETS = {
    TOKEN: '{{T}}',
    categories: [
      {
        id: 'code',
        label: 'a line of JavaScript or C',
        templates: [
          'const total = price * qty{{T}}',
          'return a + b{{T}}',
          'int count = 0{{T}}',
          'printf("hello, world\\n"){{T}}',
          'let user = await fetchUser(id){{T}}',
          'console.log(result){{T}}',
          'free(buffer){{T}}',
          'char *name = argv[1]{{T}}',
          'import { readFile } from "fs"{{T}}',
          'x += 1{{T}}'
        ]
      },
      {
        id: 'greek',
        label: 'a Greek sentence',
        templates: [
          'Τι ώρα είναι{{T}}',
          'Πού είναι ο σταθμός{{T}}',
          'Μιλάτε αγγλικά{{T}}',
          'Πόσο κάνει αυτό{{T}}',
          'Θέλεις καφέ{{T}}',
          'Γιατί άργησες πάλι{{T}}',
          'Ποιος χτυπάει την πόρτα{{T}}'
        ]
      },
      {
        id: 'english',
        label: 'an English sentence',
        templates: [
          'I came, I saw{{T}} I conquered.',
          'It was late{{T}} the trains had stopped.',
          'She wanted to stay{{T}} he wanted to go.',
          'The build passed{{T}} nobody believed it.',
          'Some people like cats{{T}} others prefer dogs.',
          'Pack a map, a compass and water{{T}} the trail is long.'
        ]
      }
    ]
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = SNIPPETS;
  } else {
    root.GQM_SNIPPETS = SNIPPETS;
  }
})(this);
