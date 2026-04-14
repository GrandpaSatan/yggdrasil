/* Prism language: YAML */
Prism.languages.yaml = {
  'scalar': { pattern: /(\s*(?:[-?|>!'"]|\\.)?[ \t]*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*(?:&[^\s[\]{}/,]+)?[ \t]+)\|[\s\S]*/, lookbehind: true, alias: 'string' },
  'comment': /#.*/,
  'key': { pattern: /(\s*(?:^|[:\-,[{\r\n?])[ \t]*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*(?:&[^\s[\]{}/,]+)?[ \t]*)[^\s[\]{}/,!'"]+ *(?=:(?:\s|$))/, lookbehind: true, alias: 'atrule' },
  'directive': { pattern: /(^[ \t]*)%.+/m, lookbehind: true, alias: 'important' },
  'datetime': { pattern: /([:\-,[{]\s*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*)(?:\d{4}-\d\d?-\d\d?(?:[tT]|[ \t]+)\d\d?:\d{2}:\d{2}(?:\.\d*)?(?:[ \t]*(?:Z|[-+]\d\d?(?::\d{2})?))?|\d{4}-\d{2}-\d{2}|\d\d?:\d{2}(?::\d{2}(?:\.\d*)?)?)(?=[ \t]*(?:$|,|\]|\}|#))/, lookbehind: true, alias: 'number' },
  'boolean': { pattern: /([:\-,[{]\s*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*)(?:false|true)[ \t]*(?=$|,|\]|\}|#)/im, lookbehind: true, alias: 'important' },
  'null': { pattern: /([:\-,[{]\s*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*)(?:null|~)[ \t]*(?=$|,|\]|\}|#)/im, lookbehind: true, alias: 'important' },
  'string': { pattern: /([:\-,[{]\s*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*)(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')(?=[ \t]*(?:$|,|\]|\}|#))/m, lookbehind: true, greedy: true },
  'number': { pattern: /([:\-,[{]\s*(?:!(?!\s)[^\s[\]{}/,]+)?[ \t]*)[+-]?(?:0x[\da-f]+|0o[0-7]+|(?:\d+\.?\d*|\.?\d+)(?:e[+-]?\d+)?|\.inf|\.nan)[ \t]*(?=$|,|\]|\}|#)/im, lookbehind: true },
  'tag': /!(?:<[^>]+>|(?:[!a-z\xE0-\uFFFF][-\w\xE0-\uFFFF]*)?(?::[a-z\xE0-\uFFFF][-\w\xE0-\uFFFF]*)*)(?=[ \t])/i,
  'important': /[&*][\w]+/,
  'punctuation': /---|[\[\]{}\-,|>?:]+|\.\.\.|[#'"!]/,
};
Prism.languages.yml = Prism.languages.yaml;
