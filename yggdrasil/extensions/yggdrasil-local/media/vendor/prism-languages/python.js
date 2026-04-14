/* Prism language: Python */
Prism.languages.python = {
  'comment': { pattern: /(^|[^\\])#.*/, lookbehind: true, greedy: true },
  'string-interpolation': {
    pattern: /(?:f|rb?|br?)"""[\s\S]*?"""|(?:f|rb?|br?)'''[\s\S]*?'''|(?:f|rb?|br?)"(?:\\.|[^\\"\r\n])*"|(?:f|rb?|br?)'(?:\\.|[^\\'\r\n])*'/i,
    greedy: true,
    alias: 'string',
  },
  'triple-quoted-string': {
    pattern: /(?:[rub]|rb|br)?"""[\s\S]*?"""|(?:[rub]|rb|br)?'''[\s\S]*?'''/i,
    greedy: true,
    alias: 'string',
  },
  'string': {
    pattern: /(?:[rub]|rb|br)?(?:"(?:\\.|[^\\"\r\n])*"|'(?:\\.|[^\\'\r\n])*')/i,
    greedy: true,
  },
  'function': { pattern: /((?:^|\s)def[ \t]+)[a-zA-Z_]\w*(?=\s*\()/, lookbehind: true },
  'class-name': { pattern: /(\bclass\s+)\w+/, lookbehind: true },
  'decorator': { pattern: /(^[\t ]*)@\w+(?:\.\w+)*/m, lookbehind: true, alias: ['annotation', 'punctuation'] },
  'keyword': /\b(?:_(?=\s*:)|and|as|assert|async|await|break|case|class|continue|def|del|elif|else|except|exec|finally|for|from|global|if|import|in|is|lambda|match|nonlocal|not|or|pass|print|raise|return|try|while|with|yield)\b/,
  'builtin': /\b(?:__import__|abs|all|any|ascii|bin|bool|breakpoint|bytearray|bytes|callable|chr|classmethod|compile|complex|copyright|credits|delattr|dict|dir|divmod|enumerate|eval|exec|exit|filter|float|format|frozenset|getattr|globals|hasattr|hash|help|hex|id|input|int|isinstance|issubclass|iter|len|license|list|locals|map|max|memoryview|min|next|object|oct|open|ord|pow|print|property|quit|range|repr|reversed|round|set|setattr|slice|sorted|staticmethod|str|sum|super|tuple|type|vars|zip)\b/,
  'boolean': /\b(?:False|None|True)\b/,
  'number': /(?:\b(?=\d)|\B(?=\.))(?:0[bo])?\d+(?:(?!\.)__?\d+)*(?:[eEj](?:[+-]?\d+(?:_\d+)*)?)?\b|\.\d+(?:_\d+)*(?:[eEj](?:[+-]?\d+(?:_\d+)*)?)?\b/i,
  'operator': /[-+%=]=?|!=|:=|\*\*?=?|\/\/?=?|<[<=>]?|>[=>]?|[&|^~]/,
  'punctuation': /[{}[\];(),.]/,
};
