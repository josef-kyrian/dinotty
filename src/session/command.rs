//! Shared live-PTY execution and completion fanout for MCP and the agent API.

/// Distinguishes pane ownership conflicts from transport/lifecycle failures.
#[derive(Debug)]
pub enum ExecuteError {
    /// A previous command still owns this pane, even if its caller has timed out.
    Busy,
    /// The terminal closed before execution or completion.
    Closed,
    /// Input could not be delivered; a partial write may have reached the shell.
    Write(String),
}

impl std::fmt::Display for ExecuteError {
    /// Preserve actionable protocol messages without coupling the session to HTTP.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => {
                formatter.write_str("Pane is busy: the previous command has not completed")
            }
            Self::Closed => formatter.write_str("Terminal closed before command completion"),
            Self::Write(error) => write!(formatter, "Write failed: {error}"),
        }
    }
}

impl std::error::Error for ExecuteError {}

impl super::Session {
    /// Execute in the live PTY; timeout/cancellation leaves the pane reserved until completion.
    ///
    /// # Errors
    /// Rejects busy/closed panes and reports input delivery failures.
    pub async fn execute_command(
        &self,
        command: &str,
        timeout_ms: u64,
    ) -> Result<crate::vt_screen::CommandResult, ExecuteError> {
        let mut receiver = {
            // Await backend availability before taking the synchronous parser lock.
            let mut backend = self.backend.lock().await;
            if matches!(*backend, super::SessionBackend::Exited) || self.is_exited() {
                return Err(ExecuteError::Closed);
            }
            let mut screen = self.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let receiver = screen.begin_execution().map_err(|_| ExecuteError::Busy)?;
            let input = format!("{command}\n");
            let write_result = if matches!(*backend, super::SessionBackend::Ssh) {
                let sender =
                    self.ssh_cmd_tx.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                sender.as_ref().ok_or_else(|| "SSH channel closed".to_string()).and_then(|sender| {
                    sender
                        .send(super::SshCmd::Input(input.into_bytes()))
                        .map_err(|error| error.to_string())
                })
            } else {
                Self::write_input_locked(&mut backend, input.as_bytes())
            };
            if let Err(error) = write_result {
                if matches!(*backend, super::SessionBackend::Ssh) {
                    // A rejected queue send cannot have written any bytes remotely.
                    screen.abandon_execution();
                }
                // A local partial write is possible, so its reservation must remain intact.
                return Err(ExecuteError::Write(error));
            }
            receiver
        };
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), &mut receiver)
            .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(ExecuteError::Closed),
            Err(_) => {
                let screen = self.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                // Extraction and timeout are ordered by the same screen lock.
                match receiver.try_recv() {
                    Ok(result) => Ok(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        Err(ExecuteError::Closed)
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                        Ok(screen.command_timeout())
                    }
                }
            }
        }
    }

    /// Sole completion extraction API for local/SSH readers and the fallback watchdog.
    /// The caller holds the screen lock so reservation release and delivery are atomic.
    pub(crate) fn collect_command_results(&self, screen: &mut crate::vt_screen::VirtualScreen) {
        let results = screen.drain_command_results();
        if results.is_empty() {
            return;
        }
        let mut pending =
            self.pending_results.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.extend(results);
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
