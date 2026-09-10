# Bootstrap the same startup files before installing terminal integration.
if [[ ${DINOTTY_BASH_LOGIN-} == 1 ]]; then
    unset DINOTTY_BASH_LOGIN
    shopt -s login_shell
    [[ -r /etc/profile ]] && source /etc/profile
    for __dinotty_profile in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
        if [[ -r $__dinotty_profile ]]; then
            source "$__dinotty_profile"
            break
        fi
    done
    unset __dinotty_profile
else
    [[ -r ~/.bashrc ]] && source ~/.bashrc
fi

# Keep the command status intact for existing prompt hooks and custom PS1.
_dinotty_precmd() {
    local exit_code=$?
    if [[ ${__dinotty_prompt_seen-} == 1 ]]; then
        printf '\033]133;D;%d\033\\' "$exit_code"
    fi
    __dinotty_prompt_seen=1
    # Replace the in-memory list after saving; appending the full file duplicates history
    # and makes bounded-history eviction expensive. Incremental -n can skip or duplicate
    # another pane's entries depending on its order relative to our own -a.
    if history -a && [[ -n ${HISTFILE-} && -r $HISTFILE ]]; then
        history -c
        history -r
    fi
    printf '\033]133;A\033\\'
    printf '\033]0;%s@%s:%s\007' "$USER" "${HOSTNAME%%.*}" "${PWD/#$HOME/~}"
    return "$exit_code"
}

# PS0 runs once per interactive command, without taking ownership of DEBUG.
# Append C after the user's PS0 so its display does not enter command stdout.
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0=${PS0-}$'\033]133;C\033\\'
    if declare -F __bp_precmd_invoke_cmd >/dev/null; then
        # bash-preexec may defer installing PROMPT_COMMAND until the first prompt.
        precmd_functions=(_dinotty_precmd "${precmd_functions[@]}")
    elif (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
        PROMPT_COMMAND=(_dinotty_precmd "${PROMPT_COMMAND[@]}")
    else
        # Bash before 5.1 executes only the scalar PROMPT_COMMAND value.
        PROMPT_COMMAND="_dinotty_precmd${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}"
    fi
fi
