/* Prism language: TOML */
Prism.languages.toml = {
  'comment': { pattern: /#.*/, greedy: true },
  'table': { pattern: /^\s*\[(?:\[.*?\]\]|\[.*?\])\s*$/m, inside: { 'punctuation': /^\[{1,2}|\]{1,2}$/ } },
  'key': { pattern: /^[\w."-]+ *(?==)/m, inside: { 'punctuation': /\./ } },
  'string': [
    { pattern: /"""[\s\S]*?"""/, greedy: true },
    { pattern: /'''[\s\S]*?'''/, greedy: true },
    { pattern: /"(?:\\.|[^\\"\r\n])*"/, greedy: true },
    { pattern: /'[^'\r\n]*'/, greedy: true },
  ],
  'number': /(?:[+-]?(?:0x[\da-fA-F_]+|0o[0-7_]+|0b[01_]+|[\d_]+\.[\d_]*(?:[Ee][+-]?[\d_]+)?|[\d_]+(?:[Ee][+-]?[\d_]+)?|[Ii]nf|NaN))(?![\w.])/,
  'boolean': /\b(?:false|true)\b/,
  'date': /\b\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?)?\b/,
  'punctuation': /[[\]{},=]/,
};
