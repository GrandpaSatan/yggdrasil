/* Prism language: TypeScript (extends JavaScript) */
Prism.languages.typescript = Prism.languages.extend('javascript', {
  'class-name': [
    { pattern: /(\b(?:class|extends|implements|instanceof|interface|new|type)\s+)[\w.\\]+/, lookbehind: true },
    { pattern: /\b[A-Z]\w*(?=\s*[<(])/ },
  ],
  'keyword': /\b(?:abstract|as|asserts|async|await|break|case|catch|class|const|constructor|continue|debugger|declare|default|delete|do|else|enum|export|extends|finally|for|from|function|get|if|implements|import|in|instanceof|interface|is|keyof|let|module|namespace|new|of|override|package|private|protected|public|readonly|return|require|set|static|super|switch|this|throw|try|type|typeof|undefined|var|void|while|with|yield)\b/,
  'builtin': /\b(?:Array|ArrayBuffer|Boolean|DataView|Date|Error|Event|Float32Array|Float64Array|Function|Int8Array|Int16Array|Int32Array|Map|Math|Number|Object|Promise|Proxy|Reflect|RegExp|Set|String|Symbol|Uint8Array|Uint16Array|Uint32Array|WeakMap|WeakSet)\b/,
});
Prism.languages.ts = Prism.languages.typescript;
