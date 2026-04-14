/* Prism language: JSON */
Prism.languages.json = {
  'property': { pattern: /(^|[^\\])"(?:\\.|[^\\"\r\n])*"(?=\s*:)/, lookbehind: true, greedy: true },
  'string': { pattern: /(^|[^\\])"(?:\\.|[^\\"\r\n])*"(?!\s*:)/, lookbehind: true, greedy: true },
  'comment': [
    { pattern: /\/\/.*/, greedy: true },
    { pattern: /\/\*[\s\S]*?(?:\*\/|$)/, greedy: true },
  ],
  'number': /-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/,
  'punctuation': /[{}[\],]/,
  'operator': /:/,
  'boolean': /\b(?:false|true)\b/,
  'null': { pattern: /\bnull\b/, alias: 'keyword' },
};
Prism.languages.webmanifest = Prism.languages.json;
