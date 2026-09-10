//! Session-level delivery tests use the same SSH input queue as real remote panes.

#![allow(clippy::unwrap_used)]

/// Configure an in-memory SSH transport without opening a network connection.
async fn ssh_session() -> (
    std::sync::Arc<crate::session::Session>,
    tokio::sync::mpsc::UnboundedReceiver<crate::session::SshCmd>,
) {
    let session = crate::session::test_support::stub_session();
    *session.backend.lock().await = crate::session::SessionBackend::Ssh;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    *session.ssh_cmd_tx.lock().unwrap() = Some(sender);
    (session, receiver)
}

/// Mimic the local/SSH reader's atomic feed-and-extract critical section.
fn feed(session: &crate::session::Session, bytes: &[u8]) {
    let mut screen = session.screen.lock().unwrap();
    screen.feed(bytes);
    session.collect_command_results(&mut screen);
}

/// The waiter is ready before input reaches SSH and cannot consume notification copies.
#[tokio::test]
async fn ssh_input_and_completion_share_the_authoritative_result() {
    let (session, mut input) = ssh_session().await;
    let remote = async {
        let Some(crate::session::SshCmd::Input(bytes)) = input.recv().await else {
            panic!("expected SSH input");
        };
        assert_eq!(bytes, b"printf hello\n");
        feed(&session, b"\x1b]133;C\x07hello\x1b]133;D;7\x07\x1b]133;A\x07");
    };
    let (result, ()) = tokio::join!(session.execute_command("printf hello", 5000), remote);
    let result = result.unwrap();
    assert_eq!(result.exit_code, 7);
    assert_eq!(result.stdout, "hello");
    assert_eq!(result.method, "shell_integration");
    let events = session.pending_results.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].stdout, "hello");
    assert_eq!(events[0].exit_code, result.exit_code);
}

/// Dropping a caller cannot free its pane or reassign its eventual completion.
#[tokio::test]
async fn cancelled_waiter_keeps_ownership_until_completion() {
    let (session, mut input) = ssh_session().await;
    let first_session = session.clone();
    let first = tokio::spawn(async move { first_session.execute_command("first", 5000).await });
    assert!(matches!(input.recv().await, Some(crate::session::SshCmd::Input(_))));
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(matches!(
        session.execute_command("second", 5000).await,
        Err(super::ExecuteError::Busy)
    ));
    assert!(input.try_recv().is_err(), "busy requests must not write input");
    feed(&session, b"\x1b]133;C\x07old\x1b]133;D;7\x07\x1b]133;A\x07");
    let remote = async {
        assert!(matches!(input.recv().await, Some(crate::session::SshCmd::Input(_))));
        feed(&session, b"\x1b]133;C\x07new\x1b]133;D;0\x07");
    };
    let (second, ()) = tokio::join!(session.execute_command("second", 5000), remote);
    let second = second.unwrap();
    assert_eq!(second.stdout, "new");
    assert_eq!(second.exit_code, 0);
    assert_eq!(session.pending_results.lock().unwrap().len(), 2);
}

/// Session closure wakes the waiter as an error instead of reporting a false timeout.
#[tokio::test]
async fn closed_terminal_wakes_pending_execution() {
    let (session, mut input) = ssh_session().await;
    let close = async {
        assert!(matches!(input.recv().await, Some(crate::session::SshCmd::Input(_))));
        session.notify_exit_and_mark_exited("closed-pane", Some(7));
    };
    let (result, ()) = tokio::join!(session.execute_command("exit 7", 5000), close);
    assert!(matches!(result, Err(super::ExecuteError::Closed)));
}

/// An SSH queue rejection is known to have delivered no input and must not strand ownership.
#[tokio::test]
async fn closed_ssh_input_queue_does_not_leave_pane_busy() {
    let (session, input) = ssh_session().await;
    drop(input);
    assert!(matches!(
        session.execute_command("first", 5000).await,
        Err(super::ExecuteError::Write(_))
    ));
    assert!(matches!(
        session.execute_command("second", 5000).await,
        Err(super::ExecuteError::Write(_))
    ));
}
