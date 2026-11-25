# Bash completion for tmv. Loaded on demand by bash-completion.
_tmv_complete() {
    local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]}
    local cmd= candidates= word
    local -a context=()
    COMPREPLY=()
    case ${COMP_WORDS[1]} in
        run|open|report|attach|help)
            if (( COMP_CWORD > 1 )); then
                cmd=${COMP_WORDS[1]}
                context=("$cmd")
            fi ;;
    esac
    if [[ $cmd == run ]]; then
        for word in "${COMP_WORDS[@]:1:COMP_CWORD-1}"; do
            if [[ $word == -- ]]; then
                if [[ $prev == -- ]]; then
                    mapfile -t COMPREPLY < <(compgen -c -- "$cur")
                fi
                return 0
            fi
        done
    fi
    case $prev in
        --metric) candidates='virtual rss pss private shared swap' ;;
        --analysis) candidates='allocation layout' ;;
        --svg-format) candidates='treemap flame' ;;
        --at|--interval|--top|--lookup|--read) return 0 ;;
        --library|--heap-rules|--heap-trace|--heap-maps|--baseline|--save-baseline|--replay|--svg|--output) return 0 ;;
        *)
            if [[ $cur == -* ]]; then
                # Use the installed executable's public options as the source of truth.
                candidates=$(command tmv "${context[@]}" --help | sed -nE '/^[[:space:]]+-/s/^[[:space:]]*(-[[:alnum:]], )?(--[a-z-]+).*/\2/p')
                candidates+=" -h"
                [[ -n $cmd ]] || candidates+=" -V"
            elif [[ $cmd == help || ( -z $cmd && $COMP_CWORD == 1 ) ]]; then
                candidates='run attach open report help'
            fi ;;
    esac
    if [[ -n $candidates ]]; then
        mapfile -t COMPREPLY < <(compgen -W "$candidates" -- "$cur")
    fi
}
complete -o default -F _tmv_complete tmv
