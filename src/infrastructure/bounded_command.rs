use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

pub fn output(mut command: Command, timeout: Duration) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // ArcRelay is a GUI application. Console-subsystem helpers such as
        // PowerShell must not allocate a transient console window.
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr unavailable"))?;
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = stdout_tx.send(read_capped(stdout));
    });
    std::thread::spawn(move || {
        let _ = stderr_tx.send(read_capped(stderr));
    });
    let deadline = Instant::now() + timeout;
    let process_group_id = child.id();

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            terminate_process_group(process_group_id);
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("child process exceeded {} seconds", timeout.as_secs()),
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout = receive_output(&stdout_rx, deadline, "stdout").inspect_err(|_| {
        terminate_process_group(process_group_id);
    })?;
    let stderr = receive_output(&stderr_rx, deadline, "stderr").inspect_err(|_| {
        terminate_process_group(process_group_id);
    })?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_capped(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok(captured);
        }
        if captured.len() < MAX_CAPTURE_BYTES {
            let remaining = MAX_CAPTURE_BYTES - captured.len();
            captured.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
}

fn receive_output(
    receiver: &mpsc::Receiver<io::Result<Vec<u8>>>,
    deadline: Instant,
    stream: &str,
) -> io::Result<Vec<u8>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    receiver
        .recv_timeout(remaining)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{stream} pipe did not close before the command deadline"),
            ),
            mpsc::RecvTimeoutError::Disconnected => {
                io::Error::other(format!("{stream} reader stopped unexpectedly"))
            }
        })?
}

fn terminate_process_group(process_group_id: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(process_group_id as libc::pid_t), libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(100));
        libc::kill(-(process_group_id as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn captures_stdout_and_stderr() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf out; printf err >&2"]);

        let output = output(command, Duration::from_secs(1)).expect("command should finish");

        assert!(output.status.success());
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[cfg(unix)]
    #[test]
    fn terminates_a_timed_out_process() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);

        let error = output(command, Duration::from_millis(50)).expect_err("command must time out");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn background_descendant_cannot_hold_output_pipe_forever() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5 &"]);
        let started = Instant::now();

        let error = output(command, Duration::from_millis(100))
            .expect_err("inherited pipe must respect the deadline");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
