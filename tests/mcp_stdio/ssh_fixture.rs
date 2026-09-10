//! Optional loopback OpenSSH fixture, isolated from user keys and configuration.

/// Keep the daemon and its temporary credentials alive for one manual SSH session.
pub(super) struct SshFixture {
    /// Kill the daemon before removing its configuration directory.
    _daemon: super::super::ServerGuard,
    /// Own the host key, client key, and throwaway remote home.
    _directory: tempfile::TempDir,
    /// Shell input used to connect through the existing pane's PTY.
    pub command: String,
}

impl SshFixture {
    /// Enable real SSH coverage explicitly where an unprivileged sshd is available.
    pub fn start() -> super::super::TestResult<Option<Self>> {
        let Some(sshd) = std::env::var_os("DINOTTY_TEST_SSHD") else {
            return Ok(None);
        };
        let directory = tempfile::tempdir()?;
        let root = directory.path().display().to_string();
        let key = directory.path().join("key");
        let generated = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()?;
        assert!(generated.success(), "could not create isolated SSH key");
        let port = super::super::free_loopback_port()?;
        std::fs::write(directory.path().join("launcher"), format!(
            "exec env -i PATH=/usr/bin:/bin TERM=xterm-256color HOME='{root}' PS1='remote@fixture:~$ ' /bin/bash --noprofile --norc -i\n"
        ))?;
        let config = directory.path().join("sshd_config");
        std::fs::write(&config, format!(
            "ListenAddress 127.0.0.1\nPort {port}\nHostKey {root}/key\nAuthorizedKeysFile {root}/key.pub\nStrictModes no\nUsePAM no\nPasswordAuthentication no\nPidFile {root}/pid\nLogLevel ERROR\nForceCommand /bin/sh {root}/launcher\n"
        ))?;
        let mut daemon = super::super::ServerGuard {
            child: std::process::Command::new(sshd)
                .args(["-D", "-e", "-f"])
                .arg(&config)
                .stderr(std::process::Stdio::inherit())
                .spawn()?,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(daemon.child.try_wait()?.is_none(), "test sshd exited before listening");
            if std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "test sshd did not listen");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Ok(Some(Self {
            _daemon: daemon,
            _directory: directory,
            command: format!("ssh -tt -F /dev/null -p {port} -i '{root}/key' -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile='{root}/known_hosts' 127.0.0.1"),
        }))
    }
}
