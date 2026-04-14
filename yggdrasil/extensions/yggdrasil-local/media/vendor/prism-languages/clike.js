/* Prism language: C-like (base for JavaScript/TypeScript extension) */
Prism.languages.clike = {
  'comment': [
    { pattern: /(^|[^\\])\/\*[\s\S]*?(?:\*\/|$)/, lookbehind: true, greedy: true },
    { pattern: /(^|[^\\:])\/\/.*/, lookbehind: true, greedy: true },
  ],
  'string': { pattern: /(["'])(?:\\(?:\r\n|[\s\S])|(?!\1)[^\\\r\n])*\1/, greedy: true },
  'class-name': { pattern: /(\b(?:class|extends|implements|instanceof|interface|new)\s+|\bcatch\s+\()[\w.\\]+/, lookbehind: true, inside: { 'punctuation': /[.\\]/ } },
  'keyword': /\b(?:break|catch|continue|do|else|finally|for|function|if|in|instanceof|new|null|return|throw|try|typeof|var|void|while|with)\b/,
  'boolean': /\b(?:false|true)\b/,
  'function': /\b\w+(?=\()/,
  'number': /\b0x[\da-f]+\b|(?:\b\d+(?:\.\d*)?|\B\.\d+)(?:[eE][+-]?\d+)?/i,
  'operator': /[<>]=?|[!=]=?=?|--?|\+\+?|&&?|\|\|?|[?*/~^%]/,
  'punctuation': /[{}[\];(),.]/,
};
