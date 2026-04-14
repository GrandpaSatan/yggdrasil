/* Prism language: Bash / Shell */
(function(Prism) {
  var comment = { pattern: /(^|[^"{\\$])#.*/, lookbehind: true, greedy: true };
  var str = {
    'string': [
      { pattern: /\$'(?:[^'\\]|\\[\s\S])*'/, greedy: true },
      { pattern: /"(?:\\[\s\S]|\$\([^)]+\)|\$(?!\()|`[^`]+`|[^"\\`$])*"/, greedy: true },
      { pattern: /'[^']*'/, greedy: true },
    ],
  };
  var inside = {
    'bash': null,
    'variable': [
      { pattern: /\$\((?:\([^)]*\)|[^()])*\)/, greedy: true, inside: { 'variable': /^\$\(|\)$|,/ } },
      { pattern: /\$\{[^}]+\}/, inside: { 'operator': /:[-=?+]?|[!\/]|##?|%%?|\^\^?|,,?/, 'punctuation': /[\[\]]/, 'environment': { pattern: RegExp('(\\{)' + ['BASH','BASHOPTS','BASH_ALIASES','BASH_ARGC','BASH_ARGV','BASH_CMDS','BASH_COMPLETION_COMPAT_DIR','BASH_LINENO','BASH_REMATCH','BASH_SOURCE','BASH_VERSINFO','BASH_VERSION','COLORTERM','COLUMNS','COMP_WORDBREAKS','DBUS_SESSION_BUS_ADDRESS','DEFAULTS_PATH','DESKTOP_SESSION','DIRSTACK','DISPLAY','EUID','GDMSESSION','GDM_LANG','GNOME_KEYRING_CONTROL','GNOME_KEYRING_PID','GPG_AGENT_INFO','GROUPS','HISTCONTROL','HISTFILE','HISTFILESIZE','HISTSIZE','HOME','HOSTNAME','HOSTTYPE','IFS','IMSETTINGS_INTEGRATE_DESKTOP','IMSETTINGS_MODULE','LANG','LANGUAGE','LC_ADDRESS','LC_ALL','LC_IDENTIFICATION','LC_MEASUREMENT','LC_MESSAGES','LC_MONETARY','LC_NAME','LC_NUMERIC','LC_PAPER','LC_TELEPHONE','LC_TIME','LESSCLOSE','LESSOPEN','LINES','LOGNAME','LS_COLORS','MACHTYPE','MAILCHECK','MANDATORY_PATH','NO_AT_BRIDGE','OLDPWD','OPTERR','OPTIND','ORBIT_SOCKETDIR','OSTYPE','PAPERSIZE','PATH','PIPESTATUS','PPID','PS1','PS2','PS3','PS4','PWD','RANDOM','READLINE_LINE','READLINE_POINT','REPLY','SECONDS','SELINUX_INIT','SESSION','SESSIONTYPE','SHELLOPTS','SHLVL','SSH_AUTH_SOCK','TERM','UID','UPSTART_EVENTS','UPSTART_INSTANCE','UPSTART_JOB','UPSTART_SESSION','USER','WINDOWID','XAUTHORITY','XDG_CONFIG_DIRS','XDG_CURRENT_DESKTOP','XDG_DATA_DIRS','XDG_GREETER_DATA_DIR','XDG_MENU_PREFIX','XDG_RUNTIME_DIR','XDG_SEAT','XDG_SEAT_PATH','XDG_SESSION_CLASS','XDG_SESSION_DESKTOP','XDG_SESSION_ID','XDG_SESSION_PATH','XDG_SESSION_TYPE','XDG_VTNR','XMODIFIERS'].join('|') + ')(?=[^a-z])'), lookbehind: true, alias: 'constant' } } },
      { pattern: /\$(?:\w+|[#?*!@$])/ },
    ],
    'environment': { pattern: RegExp('\\$?' + ['BASH','PATH','HOME','USER','PWD','OLDPWD','TERM','SHELL','LANG','LC_ALL','EDITOR','VISUAL','MANPATH','IFS','PS1','PS2','PS3','PS4','RANDOM','LINENO','FUNCNAME','BASH_SOURCE','BASH_LINENO'].join('|')), alias: 'constant' },
    'function': { pattern: /(^|[\s;|&]|[<>]\()(?:add|apropos|apt|aptitude|apt-cache|apt-get|aspell|automysqlbackup|awk|basename|bash|bc|bconsole|bg|bzip2|cal|cat|cfdisk|chgrp|chkconfig|chmod|chown|chroot|cksum|clear|cmp|column|comm|composer|cp|cron|crontab|csplit|curl|cut|date|dc|dd|ddrescue|debootstrap|df|diff|diff3|dig|dir|dircolors|dirname|dirs|dmesg|docker|du|egrep|eject|env|ethtool|expand|expect|expr|fdformat|fdisk|fg|fgrep|file|find|fmt|fold|format|free|fsck|ftp|fuser|gawk|git|gparted|grep|groupadd|groupdel|groupmod|groups|grub-mkconfig|gzip|halt|head|hg|history|host|hostname|htop|iconv|id|ifconfig|ifdown|ifup|import|install|ip|jobs|join|kill|killall|less|link|ln|locate|logname|logrotate|look|lpc|lpr|lprint|lprintd|lprintq|lprm|ls|lsof|lynx|make|man|mc|mdadm|mkconfig|mkdir|mke2fs|mkfifo|mkfs|mkisofs|mknod|mkswap|mmv|more|most|mount|mtools|mtr|mutt|mv|nano|nc|netstat|nice|nl|nohup|notify-send|npm|nslookup|op|open|parted|passwd|paste|pathchk|ping|pkill|pnpm|podman|popd|pr|printcap|printenv|ps|pushd|pv|quota|quotacheck|quotactl|ram|rar|rcp|reboot|remsync|rename|renice|rev|rm|rmdir|rpm|rsync|scp|sed|service|sftp|sh|shellcheck|shuf|shutdown|sleep|slocate|sort|split|ssh|stat|strace|su|sudo|sum|suspend|swapon|sync|tac|tail|tar|tee|time|timeout|top|touch|tr|traceroute|tsort|tty|umount|uname|unexpand|uniq|units|unrar|unshar|unzip|update-grub|uptime|useradd|userdel|usermod|users|uudecode|uuencode|v|vcpkg|vdir|vi|vim|virsh|vmstat|wait|watch|wc|wget|whereis|which|who|whoami|write|xargs|xdg-open|yarn|yes|zcat|zcmp|zdiff|zegrep|zfgrep|zip|zless|zmore|znew)(?=$|\s|;|\|)/, lookbehind: true, alias: 'function' },
    'for-or-select': { pattern: /(\bfor\s+)\w+(?=\s+in\s)/, lookbehind: true, alias: 'variable' },
    'assign-left': { pattern: /(^|[\s;|&]|[<>]\()\w+(?:\.\w+)*(?=\+?=)/, lookbehind: true, inside: { 'environment': { pattern: RegExp('(^|[\\s;|&]|[<>]\\()' + ['BASH','PATH','HOME','USER','PWD'].join('|')), lookbehind: true, alias: 'constant' } } },
    'keyword': { pattern: /(^|[\s;|&]|[<>]\()(?:case|do|done|elif|else|esac|fi|for|function|if|in|return|select|then|until|while)(?=$|[)\s;|&])/, lookbehind: true },
    'boolean': { pattern: /(^|[\s;|&]|[<>]\()(?:false|true)(?=$|[)\s;|&])/, lookbehind: true },
    'operator': /&&|\|\||\d?\|&?|&>>?|<(?:&\d*-?|&|<<?|>?[|>]?)|>(?:&\d*-?|[|>]?)|=?&|!=(?=\s)|(?<!\$)[?*+@!](?=\()|\*|\?|\$[?*@!#-]|(?<!<)<\(|(?<!>)>\(|[+-](?=\b)/,
    'punctuation': /\$?\(\(?|\)\)?|\.\.|[{}[\];\\]/,
    'number': { pattern: /(^|\s)(?:[1-9]\d*|0)(?:[.,]\d+)*\b/, lookbehind: true },
  };
  inside.bash = Prism.languages.bash = {
    'shebang': { pattern: /^#!\s*\/.*/, alias: 'important', greedy: true },
    'comment': comment,
    ...str,
    'heredoc': { pattern: /((?:^|[^<{])<<-?\s*)(\w+)\s[\s\S]*?(?:\r?\n|\r)\2/, lookbehind: true, greedy: true, inside: { 'heredoc-punctuation': { pattern: /(\s*)\w+/, lookbehind: true } } },
    ...inside,
  };
  Prism.languages.sh = Prism.languages.bash;
  Prism.languages.shell = Prism.languages.bash;
}(Prism));
