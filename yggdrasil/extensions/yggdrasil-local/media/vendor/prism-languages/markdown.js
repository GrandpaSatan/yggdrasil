/* Prism language: Markdown */
(function(Prism) {
  function createMarkdown(md) {
    return {
      'frontmatter-block': { pattern: /(^(?:\s*[\r\n])?)---(?!.)[\s\S]*?[\r\n]---/, lookbehind: true, greedy: true, inside: { 'punctuation': /^---|---$/ } },
      'blockquote': { pattern: /^>(?:[\t ]*>)*/m, alias: 'punctuation' },
      'table': { pattern: RegExp('^\\|.+[\\r\\n]' + '(?:' + '^\\|[ \\t]*[:-]+[-| :\\t]*[\\r\\n]' + ')' + '(?:' + '^\\|.+' + ')+', 'm'), inside: { 'table-data-rows': { pattern: RegExp('(^\\|.+[\\r\\n](?:^\\|[ \\t]*[:-]+[-| :\\t]*[\\r\\n]))' + '(?:^\\|.+)+', 'm'), lookbehind: true, inside: { 'table-data': { pattern: /\|(?:[^|]|\\\|)*/, inside: md } } }, 'table-line': { pattern: /^(\|.+[\r\n])\|[ \t]*[:-]+[-| :\t]*[\r\n]/, lookbehind: true, inside: { 'punctuation': /[:-]+|[|]/ } }, 'table-header-row': { pattern: /^(\|).*[\r\n]?/, lookbehind: true, inside: { 'table-header': { pattern: /\|(?:[^|]|\\\|)*/, inside: md } } } } },
      'code': [
        { pattern: /^(?:[ ]{4}|\t).+/m, alias: 'keyword' },
        { pattern: /``.+?``|`[^`\r\n]+`/, alias: 'keyword' },
        { pattern: /^```[\s\S]*?^```$/m, greedy: true, inside: { 'code-block': { pattern: /^(```.*[\r\n])[\s\S]+/, lookbehind: true }, 'code-language': { pattern: /^(```).+/, lookbehind: true }, 'punctuation': /```/ } },
      ],
      'title': [
        { pattern: /\S.*(?:\r?\n|\r)(?:==+|--+)(?=[ \t]*$)/m, alias: 'important', inside: { 'punctuation': /==+$|--+$/ } },
        { pattern: /(^\s*)#{1,6}(?![#\s]).+/m, lookbehind: true, alias: 'important', inside: { 'punctuation': /^#{1,6}/ } },
      ],
      'hr': { pattern: /(^\s*)([*-])(?:[\t ]*\2){2,}(?=\s*$)/m, lookbehind: true, alias: 'punctuation' },
      'list': { pattern: /(^\s*)(?:[*+-]|\d+\.)(?=[\t ].)/m, lookbehind: true, alias: 'punctuation' },
      'url-reference': { pattern: /!?\[[^\]]+\]:[\t ]+(?:\S+|<(?:\\.|[^>\\])+>)(?:[\t ]+(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\((?:\\.|[^)\\])*\)))?/, inside: { 'variable': { pattern: /^(!?\[)[^\]]+/, lookbehind: true }, 'string': /(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\((?:\\.|[^)\\])*\))$/, 'punctuation': /^[\[\]!:]|[<>]/ }, alias: 'url' },
      'bold': { pattern: /(^|[^\\])(?:\*{2}(?:(?!<\*{2}>)[\s\S])+\*{2}|_{2}(?:(?!_{2})[\s\S])+_{2})/, lookbehind: true, greedy: true, inside: { 'content': { pattern: /(^\*{2}|_{2})[\s\S]+(?=\*{2}$|_{2}$)/, lookbehind: true, inside: {} } } },
      'italic': { pattern: /(^|[^\\])(?:\*(?:(?!\*)[\s\S])+\*|_(?:(?!_)[\s\S])+_)/, lookbehind: true, greedy: true, inside: { 'content': { pattern: /(^\*)[\s\S]+(?=\*$)/, lookbehind: true, inside: {} } } },
      'strike': { pattern: /(^|[^\\])~~(?:(?!~)[\s\S])+~~/, lookbehind: true, greedy: true, inside: { 'content': { pattern: /(^~~)[\s\S]+(?=~~$)/, lookbehind: true, inside: {} } } },
      'url': { pattern: /!?\[[^\]]+\](?:\([^\s)]+(?:[\t ]+"(?:\\.|[^"\\])*")?\)| ?\[[^\]\s]*\])/, inside: { 'operator': /^!/, 'content': { pattern: /(^\[)[^\]]+(?=\]$)/, lookbehind: true, inside: {} }, 'variable': { pattern: /(^\]\[)[^\]]+(?=\]$)/, lookbehind: true }, 'url': { pattern: /(^\]\()[^\s)]+(?=[\s)]$)/, lookbehind: true }, 'string': { pattern: /(^\]\((?:[^\s)]+ ))"(?:\\.|[^"\\])*"(?=\)$)/, lookbehind: true } } },
    };
  }
  var md = {};
  var markdown = createMarkdown(md);
  md.bold = markdown.bold;
  md.italic = markdown.italic;
  md.url = markdown.url;
  Prism.languages.markdown = Prism.languages.extend('markup', markdown);
  Prism.languages.md = Prism.languages.markdown;
}(Prism));
