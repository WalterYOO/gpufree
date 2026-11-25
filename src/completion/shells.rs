//! Deterministic shell adapter generation.
//!
//! The adapters are intentionally small: all command/option knowledge lives in
//! `gpufree __complete`. This keeps bash/zsh/fish from drifting away from the
//! clap tree and avoids shipping a second copy of the CLI grammar.

use super::detect::SUPPORTED_SHELLS;

const BASH: &str = r#"# gpufree completion for bash
# Split the raw line into the current word (cur), the value part (value),
# the value offset within the word (vstart), whether the value is quoted, and
# readline's replacement offset (q). Quotes and backslashes are honored, so
# values with spaces or `=` inside quotes survive.
_gpufree_parse_word() {
    # COMP_POINT is a byte offset in bash < 5 but a character index in bash >= 5,
    # while `${VAR:0:N}` always counts characters unless LC_ALL=C. Slice the
    # prefix in the shell's own unit, then hand the engine a byte offset (its
    # --point contract) computed from that prefix as bytes.
    local line
    if (( BASH_VERSINFO[0] < 5 )); then
        line=$(LC_ALL=C; printf '%s' "${COMP_LINE:0:COMP_POINT}")
    else
        line=${COMP_LINE:0:COMP_POINT}
    fi
    byte_point=$(LC_ALL=C; printf '%s' "${#line}")
    local n=${#line}
    local i=0 start=0 ch quote='' esc=0 eq=-1
    while (( i < n )); do
        ch=${line:i:1}
        if (( esc )); then
            esc=0
        elif [[ $ch == "\\" ]]; then
            [[ $quote != "'" ]] && esc=1
        elif [[ $ch == "'" ]]; then
            if [[ $quote == "'" ]]; then
                quote=''
            elif [[ -z $quote ]]; then
                quote="'"
            fi
        elif [[ $ch == '"' ]]; then
            if [[ $quote == '"' ]]; then
                quote=''
            elif [[ -z $quote ]]; then
                quote='"'
            fi
        elif [[ -z $quote ]]; then
            case $ch in
                ' '|$'\t'|';'|'|'|'&'|'('|')'|'<'|'>') start=$(( i + 1 )) ;;
                '=') eq=$i ;;
            esac
        fi
        i=$(( i + 1 ))
    done
    cur=${line:start}

    vstart=0
    if [[ $cur == -*=* ]]; then
        local head=${cur%%=*}
        vstart=$(( ${#head} + 1 ))
    fi
    value=${cur:vstart}

    # A value is only "wholly quoted" when its opening quote is still open at
    # the cursor. If it closes early (`--config="/tmp/x"/a=b/space`), the rest
    # is ordinary unquoted text: readline replaces after a later `=`, so the
    # value must go through q/splice like any mixed value, and compgen gets the
    # raw word (quotes included) exactly as for mid-value quotes.
    quoted=0
    if [[ $value == \"* || $value == \'* ]]; then
        local qchar=${value:0:1}
        local probe=${value:1}
        local closes=0 pj=0 plen=0
        plen=${#probe}
        while (( pj < plen )); do
            ch=${probe:pj:1}
            if [[ $qchar == '"' && $ch == "\\" ]]; then
                pj=$(( pj + 2 ))
                continue
            fi
            if [[ $ch == "$qchar" ]]; then
                closes=1
                break
            fi
            pj=$(( pj + 1 ))
        done
        if (( ! closes )); then
            quoted=1
            local body=${value:1}
            local out='' j=0 len=0
            len=${#body}
            while (( j < len )); do
                ch=${body:j:1}
                if [[ $ch == "\\" && $qchar == '"' ]]; then
                    j=$(( j + 1 ))
                    (( j < len )) && out+=${body:j:1}
                elif [[ $ch == "$qchar" ]]; then
                    :
                else
                    out+=$ch
                fi
                j=$(( j + 1 ))
            done
            value=$out
        fi
    fi

    # Without quotes, `=` in COMP_WORDBREAKS makes readline replace after the
    # last unquoted, unescaped `=`. Inside quotes the whole value is replaced
    # instead. `eq` was recorded by the scan above, which skips quoted and
    # backslash-escaped characters (`x\=y` is not a break point).
    q=0
    if (( ! quoted )) && [[ $COMP_WORDBREAKS == *"="* ]] && (( eq >= start )); then
        q=$(( eq - start + 1 ))
    fi

    # readline replaces that raw span, but compgen inside a completion function
    # returns dequoted filenames (`sp\ ace/a` comes back as `sp ace/a`, and
    # `"my dir"` as `my dir`), so count the kept prefix the way bash dequotes
    # it: backslash escapes, single quotes, and double quotes with their own
    # escape rules.
    splice=0
    if (( q > 0 )); then
        local raw rn k qs nx
        raw=${value:0:$(( q - vstart ))}
        rn=${#raw}
        k=0
        qs=''
        while (( k < rn )); do
            ch=${raw:k:1}
            if [[ $qs == "'" ]]; then
                if [[ $ch == "'" ]]; then
                    qs=''
                else
                    splice=$(( splice + 1 ))
                fi
                k=$(( k + 1 ))
            elif [[ $qs == '"' ]]; then
                if [[ $ch == '"' ]]; then
                    qs=''
                    k=$(( k + 1 ))
                elif [[ $ch == "\\" ]] && (( k + 1 < rn )); then
                    nx=${raw:k+1:1}
                    case $nx in
                        '$'|'`'|'"'|'\') splice=$(( splice + 1 )) ;;
                        *) splice=$(( splice + 2 )) ;;
                    esac
                    k=$(( k + 2 ))
                else
                    splice=$(( splice + 1 ))
                    k=$(( k + 1 ))
                fi
            else
                case $ch in
                    "\\")
                        if (( k + 1 < rn )); then
                            k=$(( k + 2 ))
                        else
                            k=$(( k + 1 ))
                        fi
                        splice=$(( splice + 1 ))
                        ;;
                    "'"|'"')
                        qs=$ch
                        k=$(( k + 1 ))
                        ;;
                    *)
                        splice=$(( splice + 1 ))
                        k=$(( k + 1 ))
                        ;;
                esac
            fi
        done
    fi
}

_gpufree_completion() {
    local out kind cur q vstart value quoted entry result byte_point splice
    COMPREPLY=()
    # Parse before calling the engine: it needs a byte point, which only the
    # COMP_POINT normalization above can produce.
    _gpufree_parse_word
    out=$(command "${GPUFREE_COMPLETION_BIN:-gpufree}" __complete \
        --shell bash --line "$COMP_LINE" --point "$byte_point" 2>/dev/null) || return 0
    [ -n "$out" ] || return 0
    kind=${out%%$'\n'*}
    if [[ "$out" == *$'\n'* ]]; then
        out=${out#*$'\n'}
    else
        out=""
    fi

    case "$kind" in
        *kind=file*|*kind=dir*)
            local mode=-f
            [[ "$kind" == *kind=dir* ]] && mode=-d
            # Complete the whole value ourselves: the shell's default fallback
            # splits at the last `=`, which breaks paths containing `=`.
            while IFS= read -r result; do
                [ -n "$result" ] || continue
                # Readline cannot stat a partial `=`-split match, so mark
                # directories itself. Inside quotes it marks them its own way
                # (closing the quote and appending `/`), so leave those alone.
                if (( ! quoted )) && [[ -d "$result" && "$result" != */ ]]; then
                    result=$result/
                fi
                if (( quoted )); then
                    entry=$result
                elif (( q > 0 )); then
                    entry=${result:$splice}
                else
                    entry=${cur:0:$vstart}$result
                fi
                [ -n "$entry" ] || continue
                COMPREPLY+=("$entry")
            done < <(compgen $mode -- "$value")
            return 0
            ;;
    esac

    while IFS= read -r result; do
        [ -n "$result" ] || continue
        if (( quoted )); then
            entry=${result:$vstart}
        elif (( q > 0 )) && (( ${#result} >= q )); then
            entry=${result:$q}
        else
            entry=$result
        fi
        [ -n "$entry" ] || continue
        COMPREPLY+=("$entry")
    done <<< "$out"
    return 0
}
complete -o filenames -F _gpufree_completion gpufree
"#;

const ZSH: &str = r#"#compdef gpufree
# gpufree completion for zsh
_gpufree() {
    local -a out candidates argv
    local kind
    argv=("${words[@]}")
    (( CURRENT > ${#words} )) && argv+=("")
    out=("${(@f)$(command "${GPUFREE_COMPLETION_BIN:-gpufree}" __complete \
        --shell zsh --cword $((CURRENT - 1)) -- "${argv[@]}" 2>/dev/null)}")
    (( ${#out} )) || return 0
    kind=${out[1]}
    candidates=("${(@)out[2,-1]}")
    case "$kind" in
        *kind=file*)
            # For `--opt=path`, strip only the option separator so paths that
            # themselves contain `=` still complete.
            if [[ ${words[CURRENT]} == --*=* ]]; then
                setopt localoptions extendedglob
                compset -P '--[^=]#='
            fi
            _files ;;
        *kind=dir*)
            if [[ ${words[CURRENT]} == --*=* ]]; then
                setopt localoptions extendedglob
                compset -P '--[^=]#='
            fi
            _directories ;;
        *) (( ${#candidates} )) && compadd -- "${candidates[@]}" ;;
    esac
}
if [[ $zsh_eval_context[-1] == loadautofunc ]]; then
    # autoloaded from fpath: run the completion function directly
    _gpufree "$@"
else
    # sourced from an rc file: register the function for later
    if (( $+functions[compdef] )); then
        compdef _gpufree gpufree
    else
        autoload -Uz compinit && compinit
        compdef _gpufree gpufree
    fi
fi
"#;

const FISH: &str = r#"# gpufree completion for fish
function __gpufree_complete
    set -l tokens (commandline -opc)
    set -l current (commandline -ct)
    set -l bin gpufree
    if set -q GPUFREE_COMPLETION_BIN
        set bin $GPUFREE_COMPLETION_BIN
    end
    set -l out (command $bin __complete --shell fish \
        --cword (count $tokens) -- $tokens "$current" 2>/dev/null)
    test (count $out) -gt 0; or return 0
    set -l kind $out[1]
    # `-f` disables fish's default file completion, so file/dir values have to
    # be completed explicitly. For `--opt=path`, fish hands us the whole
    # token: strip the prefix, complete the path, then put the prefix back.
    set -l prefix ''
    set -l target $current
    if string match -q -- '-*=*' $current
        set prefix (string replace -r '^([^=]*=).*' '$1' -- $current)
        set target (string replace -r '^[^=]*=' '' -- $current)
    end
    if string match -q '*kind=file*' -- $kind
        for line in (__fish_complete_path $target)
            set -l parts (string split -m 1 (printf '\t') -- $line)
            printf '%s%s' $prefix $parts[1]
            if set -q parts[2]
                printf '\t%s' $parts[2]
            end
            printf '\n'
        end
    else if string match -q '*kind=dir*' -- $kind
        for line in (__fish_complete_directories $target)
            set -l parts (string split -m 1 (printf '\t') -- $line)
            printf '%s%s' $prefix $parts[1]
            if set -q parts[2]
                printf '\t%s' $parts[2]
            end
            printf '\n'
        end
    else if test (count $out) -gt 1
        for candidate in $out[2..-1]
            printf '%s\n' $candidate
        end
    end
end

complete -c gpufree -f -a '(__gpufree_complete)'
"#;

/// Return the adapter for `shell` with exactly one trailing newline.
pub fn generate(shell: &str) -> String {
    let script = match shell {
        "bash" => BASH,
        "zsh" => ZSH,
        "fish" => FISH,
        _ => unreachable!("unsupported shell {shell:?}"),
    };
    debug_assert!(SUPPORTED_SHELLS.contains(&shell));
    let mut text = script.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_end_with_exactly_one_newline() {
        for shell in SUPPORTED_SHELLS {
            let script = generate(shell);
            assert!(script.ends_with('\n'), "{shell}");
            assert!(!script.ends_with("\n\n"), "{shell}");
        }
    }

    #[test]
    fn scripts_call_the_hidden_endpoint() {
        for shell in SUPPORTED_SHELLS {
            let script = generate(shell);
            assert!(script.contains("__complete"), "{shell}");
            assert!(script.contains("GPUFREE_COMPLETION_BIN"), "{shell}");
            assert!(script.contains("gpufree"), "{shell}");
        }
    }

    #[test]
    fn kinds_are_delegated_to_shell_primitives() {
        let bash = generate("bash");
        assert!(bash.contains("*kind=file*|*kind=dir*"));
        assert!(bash.contains("COMP_WORDBREAKS"));
        assert!(bash.contains("_gpufree_parse_word"));
        assert!(bash.contains("quoted"));
        assert!(bash.contains("compgen"));
        assert!(!bash.contains("-o default"), "no filename fallback");
        assert!(generate("zsh").contains("compset -P"));
        assert!(generate("zsh").contains("--[^=]#="));
        assert!(generate("fish").contains("complete -c gpufree -f"));
        assert!(generate("zsh").contains("_files"));
        assert!(generate("fish").contains("__fish_complete_path"));
    }
}
