/* Prism language: Rust */
Prism.languages.rust = {
  'comment': [
    { pattern: /\/\/!.+|\/\/(?!\/)\/.+/, alias: 'doc-comment' },
    { pattern: /(^|[^\\])\/\*[\s\S]*?(?:\*\/|$)/, lookbehind: true },
    { pattern: /(^|[^\\:])\/\/.*/, lookbehind: true }
  ],
  'string': [
    { pattern: /b?"(?:\\[\s\S]|[^\\"])*"(?:#*)/, greedy: true },
    { pattern: /b?r(#*)"[\s\S]*?"\1/, greedy: true },
    { pattern: /b'(?:\\(?:x[0-7][\da-fA-F]|u\{(?:[\da-fA-F]_*)+\}|.)|[^\\\n\t'])'/, greedy: true }
  ],
  'attr-name': { pattern: /#!?\[(?:[^\[\]"]|"(?:\\[\s\S]|[^\\"])*")*\]/, alias: 'attribute' },
  'keyword': /\b(?:abstract|as|async|await|become|box|break|const|continue|crate|do|dyn|else|enum|extern|final|fn|for|if|impl|in|let|loop|macro|match|mod|move|mut|override|priv|pub|ref|return|self|Self|static|struct|super|trait|try|type|typeof|union|unsafe|unsized|use|virtual|where|while|yield)\b/,
  'number': /\b(?:0x[\da-fA-F](?:_?[\da-fA-F])*|0o[0-7](?:_?[0-7])*|0b[01](?:_?[01])*|(?:\d(?:_?\d)*)?\.?\d(?:_?\d)*(?:[Ee][+-]?\d+)?)(?:_?(?:[iu](?:8|16|32|64|128|size)?|f32|f64))?\b/,
  'boolean': /\b(?:false|true)\b/,
  'punctuation': /->|\.\.=|\.{1,3}|::|[{}[\];(),:]/,
  'operator': /[-+*\/%!^&|<>=?@~]+/,
  'function': { pattern: /\b[a-z_]\w*(?=\s*(?:::\s*<.*>)?\s*\()/, alias: 'function' },
  'macro': { pattern: /\b\w+!/, alias: 'macro' },
  'class-name': /\b[A-Z]\w*\b/,
  'lifetime': { pattern: /'\w+/, alias: 'symbol' },
};
