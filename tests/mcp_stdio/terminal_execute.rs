//! Real HTTP and stdio execution tests share the running server and its live PTYs.

#[path = "ssh_fixture.rs"]
mod ssh_fixture;

/// Wait for an actually rendered prompt, excluding command echo and old scrollback.
async fn wait_for_prompt(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    pane: &str,
    prompt: &str,
) -> super::TestResult {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let screen = super::mcp_text(
            client,
            base,
            token,
            30,
            "terminal_read",
            serde_json::json!({"pane_id":pane}),
        )
        .await?;
        if screen.trim_end().ends_with(prompt) {
            // Prompt fallback intentionally requires a short quiet period.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            return Ok(());
        }
        assert!(std::time::Instant::now() < deadline, "missing {prompt}: {screen}");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Exercise manual login, remote state, timeout ownership, and return to local hooks.
async fn exercise_nested_shell(command: &str) -> super::TestResult {
    let home = shell_home()?;
    let suffix = super::unique_suffix();
    let token = "mcp-manual-nested-shell";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some("/bin/bash"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created =
        super::mcp_call(&client, &base, token, 1, "tab_create", serde_json::json!({})).await?;
    let pane = created["pane_id"].as_str().unwrap();
    execute(&client, &base, token, pane, ":", 5000).await?;
    super::mcp_text(
        &client,
        &base,
        token,
        31,
        "terminal_send",
        serde_json::json!({"pane_id":pane,"command":command}),
    )
    .await?;
    wait_for_prompt(&client, &base, token, pane, "remote@fixture:~$").await?;
    let result = execute(
        &client,
        &base,
        token,
        pane,
        "cd /; export DINOTTY_REMOTE_VALUE=retained; printf 'REMOTE_OK\\n'",
        5000,
    )
    .await?;
    assert_eq!(result["method"], "prompt_detection", "{result}");
    assert_eq!(result["exit_code"], -1, "{result}");
    assert!(result["stdout"].as_str().unwrap().contains("REMOTE_OK\n"), "{result}");
    let result = execute(
        &client,
        &base,
        token,
        pane,
        "printf 'state=%s:%s\\n' \"$PWD\" \"$DINOTTY_REMOTE_VALUE\"",
        5000,
    )
    .await?;
    assert!(result["stdout"].as_str().unwrap().contains("state=/:retained\n"), "{result}");
    let result =
        execute(&client, &base, token, pane, "sleep 0.5; printf 'LATE_REMOTE\\n'", 50).await?;
    assert_eq!(result["method"], "timeout", "{result}");
    let error = execute(&client, &base, token, pane, "printf POISON", 5000).await.unwrap_err();
    assert!(error.to_string().contains("busy"), "{error}");
    wait_for_prompt(&client, &base, token, pane, "remote@fixture:~$").await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match execute(&client, &base, token, pane, "printf 'NEXT_REMOTE\\n'", 5000).await {
            Ok(result) => {
                assert_eq!(result["method"], "prompt_detection", "{result}");
                assert!(result["stdout"].as_str().unwrap().contains("NEXT_REMOTE\n"), "{result}");
                assert!(!result["stdout"].as_str().unwrap().contains("LATE_REMOTE"), "{result}");
                break;
            }
            Err(error) => {
                assert!(error.to_string().contains("busy"), "{error}");
                assert!(std::time::Instant::now() < deadline, "remote completion was lost");
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }
    // Sending exit manually mirrors the browser and leaves the outer SSH result unowned.
    super::mcp_text(
        &client,
        &base,
        token,
        32,
        "terminal_send",
        serde_json::json!({"pane_id":pane,"command":"exit"}),
    )
    .await?;
    wait_for_prompt(&client, &base, token, pane, "custom>").await?;
    let result =
        execute(&client, &base, token, pane, "printf 'LOCAL_OK\\n'; bash -c 'exit 7'", 5000)
            .await?;
    assert_eq!(result["method"], "shell_integration", "{result}");
    assert_eq!(result["exit_code"], 7, "{result}");
    assert_eq!(result["stdout"], "LOCAL_OK\n", "{result}");
    Ok(())
}

/// Always cover the nested-shell boundary, including hosts without an SSH daemon.
#[tokio::test]
async fn manually_opened_nested_shell_accepts_execute() -> super::TestResult {
    exercise_nested_shell("env PS1='remote@fixture:~$ ' /bin/bash --noprofile --norc -i").await
}

/// Opt-in real OpenSSH login uses the same public MCP path as the local shell fixture.
#[tokio::test]
async fn manually_opened_ssh_accepts_execute() -> super::TestResult {
    let Some(ssh) = ssh_fixture::SshFixture::start()? else {
        eprintln!("skipping loopback SSH fixture: set DINOTTY_TEST_SSHD");
        return Ok(());
    };
    exercise_nested_shell(&ssh.command).await
}

/// Repeated prompts retain shared history without duplicating memory or disk entries.
#[tokio::test]
async fn prompt_history_sync_does_not_duplicate_entries() -> super::TestResult {
    let home = shell_home()?;
    let mut history = String::new();
    for index in 0..2000 {
        std::fmt::Write::write_fmt(&mut history, format_args!(": history_entry_{index}\n"))?;
    }
    std::fs::write(home.path().join(".bash_history"), history)?;
    let rc = home.path().join(".bashrc");
    let config = std::fs::read_to_string(&rc)?;
    std::fs::write(&rc, format!("{config}\nHISTSIZE=100000\nHISTFILESIZE=200000\n"))?;
    let suffix = super::unique_suffix();
    let token = "mcp-history-regression";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some("/bin/bash"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created =
        super::mcp_call(&client, &base, token, 1, "tab_create", serde_json::json!({})).await?;
    let pane = created["pane_id"].as_str().unwrap();
    execute(&client, &base, token, pane, ":", 5000).await?;
    for _ in 0..3 {
        super::mcp_text(
            &client,
            &base,
            token,
            33,
            "terminal_send",
            serde_json::json!({"pane_id":pane,"command":""}),
        )
        .await?;
        wait_for_prompt(&client, &base, token, pane, "custom>").await?;
    }
    let result = execute(
        &client,
        &base,
        token,
        pane,
        "history | /bin/grep -Ec '^ *[0-9]+ +: history_entry_[0-9]+$'",
        5000,
    )
    .await?;
    assert_eq!(result["stdout"], "2000\n", "{result}");
    let mut file =
        std::fs::OpenOptions::new().append(true).open(home.path().join(".bash_history"))?;
    std::io::Write::write_all(&mut file, b": history_entry_2000\n")?;
    execute(&client, &base, token, pane, ":", 5000).await?;
    let result = execute(
        &client,
        &base,
        token,
        pane,
        "history | /bin/grep -Ec '^ *[0-9]+ +: history_entry_[0-9]+$'",
        5000,
    )
    .await?;
    assert_eq!(result["stdout"], "2001\n", "{result}");
    let saved = std::fs::read_to_string(home.path().join(".bash_history"))?;
    assert_eq!(saved.lines().filter(|line| line.starts_with(": history_entry_")).count(), 2001);
    Ok(())
}

/// Exercise a custom prompt, `PROMPT_COMMAND` array, and an independent DEBUG trap.
fn shell_home() -> super::TestResult<tempfile::TempDir> {
    let home = tempfile::tempdir()?;
    std::fs::write(home.path().join(".bash_profile"), "source ~/.bashrc\n")?;
    std::fs::write(
        home.path().join(".bashrc"),
        r"
PS1=$'CUSTOM FIRST LINE\ncustom> '
PROMPT_COMMAND=('USER_PROMPT_STATUS=$?' 'USER_PROMPT_CALLS=$((USER_PROMPT_CALLS + 1))')
trap 'USER_DEBUG_CALLS=$((USER_DEBUG_CALLS + 1))' DEBUG
export DINOTTY_STARTUP_VALUE=preserved
user_function() { printf 'FUNCTION_OK\n'; }
",
    )?;
    Ok(home)
}

/// Execute through the public MCP HTTP endpoint with an explicit pane and deadline.
async fn execute(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    pane: &str,
    command: &str,
    timeout: u64,
) -> super::TestResult<serde_json::Value> {
    super::mcp_call(
        client,
        base,
        token,
        10,
        "terminal_execute",
        serde_json::json!({
            "pane_id": pane, "command": command, "timeout": timeout,
        }),
    )
    .await
    .map_err(|error| format!("command {command:?}: {error}").into())
}

/// HTTP completion preserves live shell state, output boundaries, status, and duration.
#[tokio::test]
async fn http_execute_preserves_live_shell_state_and_status() -> super::TestResult {
    let home = shell_home()?;
    let suffix = super::unique_suffix();
    let token = "mcp-execute-regression";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some("/bin/bash"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created =
        super::mcp_call(&client, &base, token, 1, "tab_create", serde_json::json!({})).await?;
    let pane = created["pane_id"].as_str().unwrap();

    // Deliberately execute immediately: the waiter must survive the initial prompt.
    let result = execute(&client, &base, token, pane, "printf 'DINOTTY_EXEC_OK\\n'", 5000).await?;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["stdout"], "DINOTTY_EXEC_OK\n", "{result}");
    assert_eq!(result["method"], "shell_integration", "{result}");
    assert!(result["duration_ms"].as_u64().unwrap() < 4000, "{result}");

    let result = execute(&client, &base, token, pane, "bash -c 'exit 7'", 5000).await?;
    assert_eq!(result["exit_code"], 7, "{result}");
    let result = execute(&client, &base, token, pane,
        "printf 'status=%s env=%s\\n' \"$USER_PROMPT_STATUS\" \"$DINOTTY_STARTUP_VALUE\"; user_function; test \"$USER_DEBUG_CALLS\" -gt 0 && test \"$USER_PROMPT_CALLS\" -gt 0", 5000).await?;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(
        result["stdout"].as_str().unwrap().contains("status=7 env=preserved\nFUNCTION_OK\n"),
        "{result}"
    );

    execute(&client, &base, token, pane, "cd /; export DINOTTY_LIVE_VALUE=retained", 5000).await?;
    let result = execute(
        &client,
        &base,
        token,
        pane,
        "printf '%s:%s\\n' \"$PWD\" \"$DINOTTY_LIVE_VALUE\"",
        5000,
    )
    .await?;
    assert_eq!(result["stdout"], "/:retained\n", "{result}");

    let result =
        execute(&client, &base, token, pane, "sleep 0.2; printf '\\033[31mDONE\\033[0m\\n'", 5000)
            .await?;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["stdout"], "DONE\n", "{result}");
    assert!((150..4000).contains(&result["duration_ms"].as_u64().unwrap()), "{result}");

    // The stdio transport must reach the same corrected core and live shell state.
    super::set_mcp(&client, &base, token, true, true).await?;
    let request = serde_json::json!({"jsonrpc":"2.0", "id":23, "method":"tools/call", "params":{
        "name":"terminal_execute", "arguments":{"pane_id":pane,"command":"printf '%s\\n' \"$DINOTTY_LIVE_VALUE\"; bash -c 'exit 7'","timeout":5000}
    }}).to_string();
    let (code, lines, stderr) = super::run_stdio_proxy_with_home(
        base.rsplit(':').next().unwrap().parse()?,
        token,
        &suffix,
        &[&request],
        Some(home.path()),
    )?;
    assert_eq!(code, 0, "{stderr}");
    let response: serde_json::Value = serde_json::from_str(&lines[0])?;
    let result: serde_json::Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())?;
    assert_eq!(result["exit_code"], 7, "{result}");
    assert_eq!(result["stdout"], "retained\n", "{result}");
    assert_eq!(result["method"], "shell_integration");
    Ok(())
}

/// Busy rejection survives timeout while other panes remain independent.
#[tokio::test]
async fn timeout_and_concurrency_cannot_cross_wire_results() -> super::TestResult {
    let home = shell_home()?;
    let suffix = super::unique_suffix();
    let token = "mcp-execute-concurrency";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some("/bin/bash"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created =
        super::mcp_call(&client, &base, token, 1, "tab_create", serde_json::json!({})).await?;
    let pane = created["pane_id"].as_str().unwrap();
    let split = super::mcp_call(
        &client,
        &base,
        token,
        2,
        "pane_split",
        serde_json::json!({"tab_id":created["tab_id"]}),
    )
    .await?;
    let other = split["new_pane_id"].as_str().unwrap();
    execute(&client, &base, token, pane, ":", 5000).await?;
    execute(&client, &base, token, other, ":", 5000).await?;

    let result = execute(
        &client,
        &base,
        token,
        pane,
        "printf 'START\\n'; while [ ! -f \"$HOME/timeout-release\" ]; do sleep 0.02; done; printf 'LATE\\n'; bash -c 'exit 7'",
        100,
    )
    .await?;
    assert_eq!(result["method"], "timeout", "{result}");
    assert_eq!(result["exit_code"], -1, "{result}");
    assert_eq!(result["stdout"], "START\n", "{result}");
    assert!((80..4000).contains(&result["duration_ms"].as_u64().unwrap()), "{result}");
    let error = execute(&client, &base, token, pane, "printf POISON", 5000).await.unwrap_err();
    assert!(error.to_string().contains("busy"), "{error}");
    let result = execute(&client, &base, token, other, "printf 'OTHER\\n'", 5000).await?;
    assert_eq!(result["stdout"], "OTHER\n", "{result}");
    assert_eq!(result["exit_code"], 0);

    std::fs::write(home.path().join("timeout-release"), "release")?;
    // Retry only busy responses; eventually A finishes and B gets its own result.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match execute(&client, &base, token, pane, "printf 'NEXT\\n'", 5000).await {
            Ok(result) => {
                assert_eq!(result["stdout"], "NEXT\n", "{result}");
                assert_eq!(result["exit_code"], 0, "{result}");
                break;
            }
            Err(error) => {
                assert!(error.to_string().contains("busy"), "{error}");
                assert!(std::time::Instant::now() < deadline, "late command never completed");
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }

    let first = execute(&client, &base, token, pane,
        ": > \"$HOME/concurrent-started\"; while [ ! -f \"$HOME/concurrent-release\" ]; do sleep 0.02; done; printf 'FIRST\\n'", 5000);
    let concurrent = async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !home.path().join("concurrent-started").exists() {
            assert!(std::time::Instant::now() < deadline, "first command did not start");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let error = execute(&client, &base, token, pane, "printf WRONG", 5000).await.unwrap_err();
        assert!(error.to_string().contains("busy"), "{error}");
        let result = execute(&client, &base, token, other, "printf 'PARALLEL\\n'", 5000).await;
        std::fs::write(home.path().join("concurrent-release"), "release")?;
        result
    };
    let (first, parallel) = tokio::join!(first, concurrent);
    assert_eq!(first?["stdout"], "FIRST\n");
    assert_eq!(parallel?["stdout"], "PARALLEL\n");
    Ok(())
}

/// A plain shell without any OSC hook must complete through the heuristic fallback.
#[tokio::test]
async fn plain_shell_uses_prompt_fallback() -> super::TestResult {
    let home = shell_home()?;
    let suffix = super::unique_suffix();
    let token = "mcp-execute-fallback";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some("/bin/sh"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created = super::mcp_call(
        &client,
        &base,
        token,
        1,
        "tab_create",
        serde_json::json!({"argv":["/bin/sh","-i"]}),
    )
    .await?;
    let pane = created["pane_id"].as_str().unwrap();
    let result = execute(&client, &base, token, pane, "printf 'FALLBACK_OK\\n'", 5000).await?;
    assert_eq!(result["method"], "prompt_detection", "{result}");
    assert_eq!(result["exit_code"], -1, "{result}");
    assert!(result["stdout"].as_str().unwrap().contains("FALLBACK_OK\n"), "{result}");
    assert!(!result["stdout"].as_str().unwrap().contains('\u{1b}'));
    Ok(())
}

/// A delayed bash-preexec-style installer owns `PROMPT_COMMAND` and DEBUG throughout.
#[tokio::test]
async fn bash_preexec_hooks_coexist_with_command_tracking() -> super::TestResult {
    let home = shell_home()?;
    let rc = home.path().join(".bashrc");
    let mut config = std::fs::read_to_string(&rc)?;
    config.push_str(
        r#"
# Model bash-preexec's public hook arrays and delayed first-prompt installation.
precmd_functions=(user_precmd)
preexec_functions=(user_preexec)
user_precmd() { USER_PROMPT_STATUS=$?; }
user_preexec() { USER_PREEXEC_CALLS=$((USER_PREEXEC_CALLS + 1)); }
__bp_restore_status() { return "$1"; }
__bp_precmd_invoke_cmd() {
    local exit_code=$?
    local hook
    for hook in "${precmd_functions[@]}"; do
        __bp_restore_status "$exit_code"
        "$hook"
    done
    return "$exit_code"
}
__bp_preexec_invoke_exec() {
    local hook
    for hook in "${preexec_functions[@]}"; do "$hook"; done
}
__bp_install() {
    PROMPT_COMMAND=('__bp_precmd_invoke_cmd' 'USER_PROMPT_CALLS=$((USER_PROMPT_CALLS + 1))')
    trap '__bp_preexec_invoke_exec' DEBUG
    __bp_precmd_invoke_cmd
}
PROMPT_COMMAND=__bp_install
"#,
    );
    if let Ok(preexec) = std::env::var("DINOTTY_TEST_BASH_PREEXEC") {
        // Optional real upstream implementation supplements the self-contained fixture.
        std::fs::copy(preexec, home.path().join("bash-preexec.sh"))?;
        config = std::fs::read_to_string(&rc)?;
        config.push_str(
            r#"
source "$HOME/bash-preexec.sh"
user_precmd() { USER_PROMPT_STATUS=$?; }
user_preexec() { USER_PREEXEC_CALLS=$((USER_PREEXEC_CALLS + 1)); }
precmd_functions+=(user_precmd)
preexec_functions+=(user_preexec)
"#,
        );
    }
    std::fs::write(rc, config)?;
    let suffix = super::unique_suffix();
    let token = "mcp-execute-bash-preexec";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some("/bin/bash"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created =
        super::mcp_call(&client, &base, token, 1, "tab_create", serde_json::json!({})).await?;
    let pane = created["pane_id"].as_str().unwrap();
    let result = execute(&client, &base, token, pane, "bash -c 'exit 7'", 5000).await?;
    assert_eq!(result["exit_code"], 7, "{result}");
    assert_eq!(result["method"], "shell_integration", "{result}");
    let result = execute(&client, &base, token, pane,
        "printf 'status=%s\\n' \"$USER_PROMPT_STATUS\"; trap -p DEBUG; test \"$USER_PREEXEC_CALLS\" -gt 0", 5000).await?;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(result["stdout"].as_str().unwrap().contains("status=7\n"), "{result}");
    assert!(result["stdout"].as_str().unwrap().contains("__bp_preexec_invoke_exec"), "{result}");
    Ok(())
}

/// Zsh status must survive user precmd functions and prompt metadata emission.
#[tokio::test]
async fn zsh_execute_preserves_nonzero_status() -> super::TestResult {
    let shell = std::env::var("DINOTTY_TEST_ZSH").unwrap_or_else(|_| "/bin/zsh".to_string());
    if !std::path::Path::new(&shell).is_file() {
        eprintln!("Skipping Zsh regression: install zsh or set DINOTTY_TEST_ZSH");
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join(".zshrc"),
        r"
PROMPT=$'CUSTOM ZSH\n%# '
precmd() { USER_PROMPT_STATUS=$?; true; }
user_precmd() { USER_PROMPT_CALLS=$((USER_PROMPT_CALLS + 1)); }
precmd_functions=(user_precmd)
",
    )?;
    if let Ok(modules) = std::env::var("DINOTTY_TEST_ZSH_MODULES") {
        std::fs::write(
            home.path().join(".zshenv"),
            format!("module_path=('{}')\n", modules.replace('\'', "'\\''")),
        )?;
    }
    let suffix = super::unique_suffix();
    let token = "mcp-execute-zsh";
    let (_guard, base) =
        super::spawn_server_with_environment(token, &suffix, Some(home.path()), Some(&shell))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    super::wait_until_ready(&client, &base).await?;
    let created =
        super::mcp_call(&client, &base, token, 1, "tab_create", serde_json::json!({})).await?;
    let pane = created["pane_id"].as_str().unwrap();
    let result =
        execute(&client, &base, token, pane, "printf 'ZSH_OK\\n'; bash -c 'exit 7'", 5000).await?;
    assert_eq!(result["exit_code"], 7, "{result}");
    assert_eq!(result["stdout"], "ZSH_OK\n", "{result}");
    assert_eq!(result["method"], "shell_integration", "{result}");
    let result = execute(
        &client,
        &base,
        token,
        pane,
        "printf 'status=%s\\n' \"$USER_PROMPT_STATUS\"; test \"$USER_PROMPT_CALLS\" -gt 0",
        5000,
    )
    .await?;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["stdout"], "status=7\n", "{result}");
    Ok(())
}
