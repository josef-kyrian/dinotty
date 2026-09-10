//! Regression coverage for parser ownership, capture boundaries, and fallback state.

#![allow(clippy::unwrap_used)]

/// Completion transfers output and its waiter atomically before the next prompt.
#[test]
fn completion_keeps_status_output_and_waiter() {
    for ending in
        [b"\x1b]133;D;7\x1b\\\x1b]133;A\x07".as_slice(), b"\x1b]133;A\x07\x1b]133;D;7\x07"]
    {
        let mut screen = super::VirtualScreen::new(80, 24);
        let mut receiver = screen.begin_execution().unwrap();
        for byte in b"\x1b[?2004l\x1b[32mhello\x1b[0m\r\n\x1b]0;hidden title\x07" {
            screen.feed(&[*byte]);
        }
        screen.feed(ending);
        screen.feed(b"next prompt $ ");
        let results = screen.drain_command_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].exit_code, 7);
        assert_eq!(results[0].stdout, "hello\n");
        assert_eq!(results[0].method, "shell_integration");
        assert!(results[0].duration_ms < 1000);
        assert_eq!(receiver.try_recv().unwrap().stdout, "hello\n");
        assert!(screen.drain_command_results().is_empty());
        assert!(screen.has_shell_integration());
    }
}

/// Startup prompt markers and an unsolicited D must not create a completion.
#[test]
fn initial_prompt_has_no_command_result() {
    let mut screen = super::VirtualScreen::new(80, 24);
    screen.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    assert!(screen.drain_command_results().is_empty());
    assert!(screen.pending_command.is_none());
    assert!(screen.has_shell_integration());
}

/// Standard B ends the prompt; C clears echo without replacing the API waiter.
#[test]
fn execution_marker_drops_echo_and_retains_duration() {
    let mut screen = super::VirtualScreen::new(80, 24);
    screen.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let mut receiver = screen.begin_execution().unwrap();
    let started = screen.pending_command.as_ref().unwrap().start_time;
    screen.feed(b"printf hello\r\n\x1b]133;C\x07hello\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    let results = screen.drain_command_results();
    assert_eq!(results[0].stdout, "hello");
    assert!(results[0].duration_ms <= started.elapsed().as_millis() as u64);
    assert_eq!(receiver.try_recv().unwrap().exit_code, 0);
}

/// A timeout snapshots output but cannot hand a late completion to the next call.
#[test]
fn timeout_retains_reservation_until_late_completion() {
    let mut screen = super::VirtualScreen::new(80, 24);
    let receiver = screen.begin_execution().unwrap();
    screen.feed(b"\x1b]133;C\x07first");
    assert_eq!(screen.command_timeout().stdout, "first");
    drop(receiver);
    assert!(screen.begin_execution().unwrap_err().contains("busy"));
    screen.feed(b"\x1b]133;D;7\x07\x1b]133;A\x07");
    assert_eq!(screen.drain_command_results()[0].exit_code, 7);
    let mut second = screen.begin_execution().unwrap();
    screen.feed(b"\x1b]133;C\x07second\x1b]133;D;0\x07");
    assert_eq!(screen.drain_command_results()[0].stdout, "second");
    assert_eq!(second.try_recv().unwrap().exit_code, 0);
}

/// Silence alone is insufficient; fallback needs fresh output and a visible prompt.
#[test]
fn fallback_requires_new_output_and_never_overrides_osc() {
    let mut screen = super::VirtualScreen::new(80, 24);
    screen.feed(b"$ ");
    let mut receiver = screen.begin_execution().unwrap();
    assert!(!screen.has_shell_integration());
    assert!(!screen.should_check_prompt());
    assert!(!screen.detect_prompt());
    screen.feed(b"echo hello\r\nhello\r\n$ ");
    screen.last_output_time =
        std::time::Instant::now().checked_sub(std::time::Duration::from_millis(150));
    assert!(screen.detect_prompt());
    let results = screen.drain_command_results();
    assert_eq!(results[0].method, "prompt_detection");
    assert_eq!(results[0].exit_code, -1);
    assert!(!results[0].stdout.ends_with("$ "));
    assert!(receiver.try_recv().unwrap().stdout.contains("hello\n"));

    screen.feed(b"\x1b]133;A\x07");
    let _receiver = screen.begin_execution().unwrap();
    screen.feed(b"\x1b]133;C\x07$ ");
    screen.last_output_time =
        std::time::Instant::now().checked_sub(std::time::Duration::from_millis(150));
    assert!(!screen.should_check_prompt());
    assert!(!screen.detect_prompt());
}

/// Prompt-only integration must still use fallback without losing a final output line.
#[test]
fn prompt_only_markers_keep_fallback_available() {
    let mut screen = super::VirtualScreen::new(80, 24);
    screen.feed(b"\x1b]133;A\x07PS C:\\> ");
    let mut receiver = screen.begin_execution().unwrap();
    screen.feed(b"\r\nhello\x1b]133;A\x07\r\nPS C:\\> ");
    screen.last_output_time =
        std::time::Instant::now().checked_sub(std::time::Duration::from_millis(150));
    assert!(!screen.has_shell_integration());
    assert!(screen.detect_prompt());
    assert_eq!(screen.drain_command_results()[0].stdout, "\nhello");
    assert_eq!(receiver.try_recv().unwrap().method, "prompt_detection");
}

/// Zsh's erased `PROMPT_SP` padding and UTF-8 overprinting are not command output.
#[test]
fn capture_handles_carriage_return_and_backspace() {
    let mut screen = super::VirtualScreen::new(80, 24);
    let _receiver = screen.begin_execution().unwrap();
    screen.feed("bad\r好xx\x08!\r\n".as_bytes());
    screen.feed(b"\x1b[1m\x1b[7m%\x1b[0m       \r \r\x1b]133;D;0\x07");
    let results = screen.drain_command_results();
    assert_eq!(results[0].stdout, "好x!\n");
}
