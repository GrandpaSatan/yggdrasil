/* Prism.js core — v1.29.0, vendored for Yggdrasil VSCode extension.
   No CDN dependency. Tokenizer + highlighter only (no auto-highlight on load).
   Language plugins loaded separately from media/vendor/prism-languages/.
   Usage: Prism.highlight(code, Prism.languages.rust, 'rust') */
var Prism = (function () {
  var lang = {};
  var P = {
    languages: lang,
    hooks: { all: {}, add: function(name, cb) { (this.all[name] = this.all[name] || []).push(cb); }, run: function(name, env) { var cbs = this.all[name]; if (!cbs || !cbs.length) return; for (var i = 0, cb; (cb = cbs[i++]);) cb(env); } },

    highlight: function(text, grammar, language) {
      var env = { code: text, grammar: grammar, language: language };
      P.hooks.run('before-tokenize', env);
      env.tokens = P.tokenize(env.code, env.grammar);
      P.hooks.run('after-tokenize', env);
      return Token.stringify(P.util.encode(env.tokens), env.language);
    },

    tokenize: function(text, grammar) {
      var rest = grammar.rest;
      if (rest) {
        for (var token in rest) grammar[token] = rest[token];
        delete grammar.rest;
      }
      var tokenList = new LinkedList();
      addAfter(tokenList, tokenList.head, text);
      matchGrammar(text, tokenList, grammar, tokenList.head, 0);
      return toArray(tokenList);
    },

    util: {
      encode: function encode(tokens) {
        if (tokens instanceof Token) return new Token(tokens.type, encode(tokens.content), tokens.alias);
        if (Array.isArray(tokens)) return tokens.map(encode);
        return tokens.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
      },
      type: function(o) { return Object.prototype.toString.call(o).slice(8, -1); },
      objId: function(obj) {
        if (!obj['__id']) Object.defineProperty(obj, '__id', { value: ++objIdCounter });
        return obj['__id'];
      },
      clone: function deepClone(o, visited) {
        visited = visited || {};
        var id, clone;
        switch (P.util.type(o)) {
          case 'Object':
            id = P.util.objId(o);
            if (visited[id]) return visited[id];
            clone = {}; visited[id] = clone;
            for (var key in o) if (o.hasOwnProperty(key)) clone[key] = deepClone(o[key], visited);
            return clone;
          case 'Array':
            id = P.util.objId(o);
            if (visited[id]) return visited[id];
            clone = []; visited[id] = clone;
            o.forEach(function(v, i) { clone[i] = deepClone(v, visited); });
            return clone;
          default: return o;
        }
      },
    },
  };

  var objIdCounter = 0;

  function Token(type, content, alias, matchedStr) {
    this.type = type;
    this.content = content;
    this.alias = alias;
    this.length = (matchedStr || '').length | 0;
  }
  Token.stringify = function stringify(o, language) {
    if (typeof o === 'string') return o;
    if (Array.isArray(o)) {
      var s = '';
      o.forEach(function(e) { s += stringify(e, language); });
      return s;
    }
    var env = { type: o.type, content: stringify(o.content, language), tag: 'span', classes: ['token', o.type], attributes: {}, language: language };
    var aliases = o.alias;
    if (aliases) {
      if (Array.isArray(aliases)) Array.prototype.push.apply(env.classes, aliases);
      else env.classes.push(aliases);
    }
    P.hooks.run('wrap', env);
    var attrs = '';
    for (var name in env.attributes) attrs += ' ' + name + '="' + (env.attributes[name] || '') + '"';
    return '<' + env.tag + ' class="' + env.classes.join(' ') + '"' + attrs + '>' + env.content + '</' + env.tag + '>';
  };

  // ── Linked list helpers ───────────────────────────────────────
  function LinkedList() { this.head = { value: '', prev: null, next: null }; this.tail = { value: '', prev: this.head, next: null }; this.head.next = this.tail; this.length = 0; }
  function addAfter(list, node, value) { var next = node.next; var newNode = { value: value, prev: node, next: next }; node.next = newNode; next.prev = newNode; list.length++; return newNode; }
  function removeRange(list, node, count) { var next = node.next; for (var i = 0; i < count && next !== list.tail; i++) { next = next.next; } node.next = next; next.prev = node; list.length -= i; return i; }
  function toArray(list) { var arr = []; var node = list.head.next; while (node !== list.tail) { arr.push(node.value); node = node.next; } return arr; }

  function matchGrammar(text, tokenList, grammar, startNode, startPos, rematch) {
    for (var token in grammar) {
      if (!grammar.hasOwnProperty(token) || !grammar[token]) continue;
      var patterns = grammar[token];
      patterns = Array.isArray(patterns) ? patterns : [patterns];
      for (var j = 0; j < patterns.length; ++j) {
        if (rematch && rematch.cause === token + ',' + j) return;
        var patternObj = patterns[j], inside = patternObj.inside, lookbehind = !!patternObj.lookbehind, greedy = !!patternObj.greedy, alias = patternObj.alias;
        if (greedy && !patternObj.pattern.global) {
          var flags = patternObj.pattern.toString().match(/[imsuy]*$/)[0];
          patternObj.pattern = RegExp(patternObj.pattern.source, flags + 'g');
        }
        var pattern = patternObj.pattern || patternObj;
        for (var currentNode = startNode.next, pos = startPos; currentNode !== tokenList.tail; pos += currentNode.value.length, currentNode = currentNode.next) {
          if (rematch && pos >= rematch.reach) break;
          var str = currentNode.value;
          if (tokenList.length > text.length) return;
          if (str instanceof Token) continue;
          var removeCount = 1, match;
          if (greedy) {
            match = matchPattern(pattern, pos, text, lookbehind);
            if (!match || match.index >= text.length) break;
            var from = match.index, to = match.index + match[0].length;
            var p = pos;
            var k = currentNode;
            for (; k !== tokenList.tail && (p < to || (typeof k.value === 'string' && !k.prev.value.greedy)); p += k.value.length, k = k.next) {
              removeCount++;
              if (p === from) str = k.value;
            }
            removeCount--;
            str = text.slice(pos, p);
            match.index -= pos;
          } else {
            match = matchPattern(pattern, 0, str, lookbehind);
            if (!match) continue;
          }
          var from = match.index, matchStr = match[0], before = str.slice(0, from), after = str.slice(from + matchStr.length);
          var reach = pos + str.length;
          if (rematch && reach > rematch.reach) rematch.reach = reach;
          var removeFrom = currentNode.prev;
          if (before) { removeFrom = addAfter(tokenList, removeFrom, before); pos += before.length; }
          removeRange(tokenList, removeFrom, removeCount);
          var wrapped = new Token(token, inside ? P.tokenize(matchStr, inside) : matchStr, alias, matchStr);
          currentNode = addAfter(tokenList, removeFrom, wrapped);
          if (after) addAfter(tokenList, currentNode, after);
          if (removeCount > 1) { var r = { cause: token + ',' + j, reach: reach }; matchGrammar(text, tokenList, grammar, currentNode.prev, pos, r); if (rematch && r.reach > rematch.reach) rematch.reach = r.reach; }
          break;
        }
      }
    }
  }

  function matchPattern(pattern, pos, text, lookbehind) {
    pattern.lastIndex = pos;
    var match = pattern.exec(text);
    if (match && lookbehind && match[1]) {
      var lb = match[1].length;
      match.index += lb;
      match[0] = match[0].slice(lb);
    }
    return match;
  }

  P.Token = Token;
  return P;
}());

if (typeof module !== 'undefined' && module.exports) { module.exports = Prism; }
if (typeof self !== 'undefined') { self.Prism = Prism; }
