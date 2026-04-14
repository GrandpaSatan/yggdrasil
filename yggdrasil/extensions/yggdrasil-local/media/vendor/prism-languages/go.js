/* Prism language: Go */
Prism.languages.go = {
  'comment': [
    { pattern: /(^|[^\\])\/\*[\s\S]*?(?:\*\/|$)/, lookbehind: true },
    { pattern: /(^|[^\\:])\/\/.*/, lookbehind: true }
  ],
  'string': [
    { pattern: /`[^`]*`/, greedy: true },
    { pattern: /"(?:\\[\s\S]|[^\\"])*"/, greedy: true },
    { pattern: /'(?:\\[\s\S]|[^\\'])*'/, greedy: true }
  ],
  'keyword': /\b(?:break|case|chan|const|continue|default|defer|else|fallthrough|for|func|go|goto|if|import|interface|map|package|range|return|select|struct|switch|type|var)\b/,
  'boolean': /\b(?:_|false|iota|nil|true)\b/,
  'number': /(?:\b0x[\da-fA-F]+(?:\.[\da-fA-F]*)?(?:[Pp][+-]?\d+)?|\b0o\d+|\b\d+(?:\.\d+)?(?:[Ee][+-]?\d+)?(?:i\b)?|\b\d*\.\d+(?:[Ee][+-]?\d+)?(?:i\b)?)\b/i,
  'operator': /[*\/%^!=]=?|\+[=+]?|-[=-]?|\|[=|]?|&(?:=|&|\^=?)?|>(?:>=?|=)?|<(?:<=?|=|-)?|:=|\.\.\./,
  'punctuation': /[{}[\];(),.]/,
  'builtin': /\b(?:append|cap|close|complex|copy|delete|imag|len|make|new|panic|print|println|real|recover)\b/,
  'function': { pattern: /\b[a-zA-Z_]\w*(?=\s*\()/, alias: 'function' },
  'class-name': /\b[A-Z]\w*\b/,
};
