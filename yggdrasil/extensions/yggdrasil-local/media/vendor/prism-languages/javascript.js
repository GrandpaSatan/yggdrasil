/* Prism language: JavaScript */
Prism.languages.javascript = Prism.languages.extend('clike', {
  'class-name': [
    { pattern: /(\b(?:class|extends|implements|instanceof|interface|new)\s+)[\w.\\]+/, lookbehind: true },
    { pattern: /\b[A-Z]\w*(?=\s*[<(])/ },
  ],
  'keyword': [
    { pattern: /((?:^|\})\s*)(?:catch|finally)\b/, lookbehind: true },
    { pattern: /(^|[^.])(?:as|async|await|break|case|class|const|continue|debugger|default|delete|do|else|enum|export|extends|for|from|function|get|if|implements|import|in|instanceof|let|new|of|package|private|protected|public|return|set|static|super|switch|this|throw|try|typeof|undefined|var|void|while|with|yield)\b/, lookbehind: true },
  ],
  'number': /\b(?:(?:0[xX])[\dA-Fa-f]+|(?:0[bB])[01]+|(?:0[oO])[0-7]+|(?:\d+\.?\d*|\.\d+)(?:[Ee][+-]?\d+)?)\b/,
  'function': { pattern: /[_$a-zA-Z\xA0-\uFFFF][$\w\xA0-\uFFFF]*(?=\s*(?:\.\s*(?:apply|bind|call)\s*)?\()/, alias: 'function' },
  'operator': /--|\+\+|\*\*=?|=>|&&=?|\|\|=?|[!=]==|<<=?|>>>?=?|[-+*/%&|^!=<>]=?|\.{3}|\?\?=?|\?\.?|[~:]/,
});
Prism.languages.insertBefore('javascript', 'function-variable', {
  'string': [
    { pattern: /`(?:\\[\s\S]|\$\{(?:[^{}]|\{(?:[^{}]|\{[^}]*\})*\})+\}|(?!\$\{)[^\\`])*`/, greedy: true, inside: { 'template-punctuation': { pattern: /^`|`$/, alias: 'string' }, 'interpolation': { pattern: /((?:^|[^\\])(?:\\{2})*)\$\{(?:[^{}]|\{(?:[^{}]|\{[^}]*\})*\})+\}/, lookbehind: true, inside: { 'interpolation-punctuation': { pattern: /^\$\{|\}$/, alias: 'punctuation' }, rest: Prism.languages.javascript } }, 'string': /[\s\S]+/ } },
    { pattern: /(["'])(?:\\(?:\r\n|[\s\S])|(?!\1)[^\\\r\n])*\1/, greedy: true },
  ],
  'comment': [
    { pattern: /\/\*[\s\S]*?(?:\*\/|$)/, greedy: true },
    { pattern: /(^|[^\\:])\/\/.*/, lookbehind: true, greedy: true },
  ],
  'regex': { pattern: /((?:^|[^$\w\xA0-\uFFFF."'\])\s])\/(?:\[(?:[^\]\\\r\n]|\\.)*\]|\\.|[^/\\\[\r\n])+\/(?=(?:\s|\/\*(?:[^*]|\*(?!\/))*\*\/)*(?:[)\],.;:{}\s]|\/\/|$))/, lookbehind: true, greedy: true, inside: { 'regex-source': { pattern: /^(\/)[\s\S]+(?=\/[dgimsuy]*$)/, lookbehind: true, alias: 'language-regex' }, 'regex-delimiter': /^\/|\/$/, 'regex-flags': /^[dgimsuy]+$/ } },
  },
  'boolean': /\b(?:false|true)\b/,
  'punctuation': /[{}[\];(),:]/,
});
Prism.languages.js = Prism.languages.javascript;
