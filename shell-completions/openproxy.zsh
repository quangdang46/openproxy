#compdef openproxy

autoload -U is-at-least

_openproxy() {
    typeset -A opt_args
    typeset -a _arguments_options
    local ret=1

    if is-at-least 5.2; then
        _arguments_options=(-s -S -C)
    else
        _arguments_options=(-s -C)
    fi

    local context curcontext="$curcontext" state line
    _arguments "${_arguments_options[@]}" : \
'--host=[]:HOST:_default' \
'--port=[]:PORT:_default' \
'--log-filter=[]:LOG_FILTER:_default' \
'--data-dir=[]:DATA_DIR:_files' \
'-h[Print help]' \
'--help[Print help]' \
":: :_openproxy_commands" \
"*::: :->openproxy" \
&& ret=0
    case $state in
    (openproxy)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-command-$line[1]:"
        case $line[1] in
            (provider)
_arguments "${_arguments_options[@]}" : \
'-h[Print help]' \
'--help[Print help]' \
":: :_openproxy__subcmd__provider_commands" \
"*::: :->provider" \
&& ret=0

    case $state in
    (provider)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-provider-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
&& ret=0
;;
(add)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
':name:_default' \
':config:_default' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__provider__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-provider-help-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(add)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
        esac
    ;;
esac
;;
(key)
_arguments "${_arguments_options[@]}" : \
'-h[Print help]' \
'--help[Print help]' \
":: :_openproxy__subcmd__key_commands" \
"*::: :->key" \
&& ret=0

    case $state in
    (key)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-key-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
&& ret=0
;;
(add)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
':name:_default' \
':key:_default' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__key__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-key-help-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(add)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
        esac
    ;;
esac
;;
(pool)
_arguments "${_arguments_options[@]}" : \
'-h[Print help]' \
'--help[Print help]' \
":: :_openproxy__subcmd__pool_commands" \
"*::: :->pool" \
&& ret=0

    case $state in
    (pool)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-pool-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
&& ret=0
;;
(status)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
':name:_default' \
&& ret=0
;;
(create)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
':name:_default' \
':proxy_url:_default' \
&& ret=0
;;
(delete)
_arguments "${_arguments_options[@]}" : \
'--json[]' \
'-h[Print help]' \
'--help[Print help]' \
':name:_default' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__pool__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-pool-help-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(status)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(create)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(delete)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
        esac
    ;;
esac
;;
(completion)
_arguments "${_arguments_options[@]}" : \
'-h[Print help]' \
'--help[Print help]' \
':shell:(bash elvish fish powershell zsh)' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-help-command-$line[1]:"
        case $line[1] in
            (provider)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__help__subcmd__provider_commands" \
"*::: :->provider" \
&& ret=0

    case $state in
    (provider)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-help-provider-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(add)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
(key)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__help__subcmd__key_commands" \
"*::: :->key" \
&& ret=0

    case $state in
    (key)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-help-key-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(add)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
(pool)
_arguments "${_arguments_options[@]}" : \
":: :_openproxy__subcmd__help__subcmd__pool_commands" \
"*::: :->pool" \
&& ret=0

    case $state in
    (pool)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:openproxy-help-pool-command-$line[1]:"
        case $line[1] in
            (list)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(status)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(create)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(delete)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
(completion)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
        esac
    ;;
esac
}

(( $+functions[_openproxy_commands] )) ||
_openproxy_commands() {
    local commands; commands=(
'provider:' \
'key:' \
'pool:' \
'completion:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__completion_commands] )) ||
_openproxy__subcmd__completion_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy completion commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help_commands] )) ||
_openproxy__subcmd__help_commands() {
    local commands; commands=(
'provider:' \
'key:' \
'pool:' \
'completion:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__completion_commands] )) ||
_openproxy__subcmd__help__subcmd__completion_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help completion commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__help_commands] )) ||
_openproxy__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__key_commands] )) ||
_openproxy__subcmd__help__subcmd__key_commands() {
    local commands; commands=(
'list:' \
'add:' \
    )
    _describe -t commands 'openproxy help key commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__key__subcmd__add_commands] )) ||
_openproxy__subcmd__help__subcmd__key__subcmd__add_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help key add commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__key__subcmd__list_commands] )) ||
_openproxy__subcmd__help__subcmd__key__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help key list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__pool_commands] )) ||
_openproxy__subcmd__help__subcmd__pool_commands() {
    local commands; commands=(
'list:' \
'status:' \
'create:' \
'delete:' \
    )
    _describe -t commands 'openproxy help pool commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__pool__subcmd__create_commands] )) ||
_openproxy__subcmd__help__subcmd__pool__subcmd__create_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help pool create commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__pool__subcmd__delete_commands] )) ||
_openproxy__subcmd__help__subcmd__pool__subcmd__delete_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help pool delete commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__pool__subcmd__list_commands] )) ||
_openproxy__subcmd__help__subcmd__pool__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help pool list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__pool__subcmd__status_commands] )) ||
_openproxy__subcmd__help__subcmd__pool__subcmd__status_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help pool status commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__provider_commands] )) ||
_openproxy__subcmd__help__subcmd__provider_commands() {
    local commands; commands=(
'list:' \
'add:' \
    )
    _describe -t commands 'openproxy help provider commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__provider__subcmd__add_commands] )) ||
_openproxy__subcmd__help__subcmd__provider__subcmd__add_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help provider add commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__help__subcmd__provider__subcmd__list_commands] )) ||
_openproxy__subcmd__help__subcmd__provider__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy help provider list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key_commands] )) ||
_openproxy__subcmd__key_commands() {
    local commands; commands=(
'list:' \
'add:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy key commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key__subcmd__add_commands] )) ||
_openproxy__subcmd__key__subcmd__add_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy key add commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key__subcmd__help_commands] )) ||
_openproxy__subcmd__key__subcmd__help_commands() {
    local commands; commands=(
'list:' \
'add:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy key help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key__subcmd__help__subcmd__add_commands] )) ||
_openproxy__subcmd__key__subcmd__help__subcmd__add_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy key help add commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key__subcmd__help__subcmd__help_commands] )) ||
_openproxy__subcmd__key__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy key help help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key__subcmd__help__subcmd__list_commands] )) ||
_openproxy__subcmd__key__subcmd__help__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy key help list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__key__subcmd__list_commands] )) ||
_openproxy__subcmd__key__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy key list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool_commands] )) ||
_openproxy__subcmd__pool_commands() {
    local commands; commands=(
'list:' \
'status:' \
'create:' \
'delete:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy pool commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__create_commands] )) ||
_openproxy__subcmd__pool__subcmd__create_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool create commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__delete_commands] )) ||
_openproxy__subcmd__pool__subcmd__delete_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool delete commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__help_commands] )) ||
_openproxy__subcmd__pool__subcmd__help_commands() {
    local commands; commands=(
'list:' \
'status:' \
'create:' \
'delete:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy pool help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__help__subcmd__create_commands] )) ||
_openproxy__subcmd__pool__subcmd__help__subcmd__create_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool help create commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__help__subcmd__delete_commands] )) ||
_openproxy__subcmd__pool__subcmd__help__subcmd__delete_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool help delete commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__help__subcmd__help_commands] )) ||
_openproxy__subcmd__pool__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool help help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__help__subcmd__list_commands] )) ||
_openproxy__subcmd__pool__subcmd__help__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool help list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__help__subcmd__status_commands] )) ||
_openproxy__subcmd__pool__subcmd__help__subcmd__status_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool help status commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__list_commands] )) ||
_openproxy__subcmd__pool__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__pool__subcmd__status_commands] )) ||
_openproxy__subcmd__pool__subcmd__status_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy pool status commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider_commands] )) ||
_openproxy__subcmd__provider_commands() {
    local commands; commands=(
'list:' \
'add:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy provider commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider__subcmd__add_commands] )) ||
_openproxy__subcmd__provider__subcmd__add_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy provider add commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider__subcmd__help_commands] )) ||
_openproxy__subcmd__provider__subcmd__help_commands() {
    local commands; commands=(
'list:' \
'add:' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'openproxy provider help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider__subcmd__help__subcmd__add_commands] )) ||
_openproxy__subcmd__provider__subcmd__help__subcmd__add_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy provider help add commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider__subcmd__help__subcmd__help_commands] )) ||
_openproxy__subcmd__provider__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy provider help help commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider__subcmd__help__subcmd__list_commands] )) ||
_openproxy__subcmd__provider__subcmd__help__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy provider help list commands' commands "$@"
}
(( $+functions[_openproxy__subcmd__provider__subcmd__list_commands] )) ||
_openproxy__subcmd__provider__subcmd__list_commands() {
    local commands; commands=()
    _describe -t commands 'openproxy provider list commands' commands "$@"
}

if [ "$funcstack[1]" = "_openproxy" ]; then
    _openproxy "$@"
else
    compdef _openproxy openproxy
fi
