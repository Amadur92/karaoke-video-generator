//! Read process output without tying cancellation to its next log message.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub enum Stream {
    Stdout,
    Stderr,
}

pub fn run<F>(mut command: Command, cancel: Arc<AtomicBool>, mut line: F) -> Result<bool, String>
where
    F: FnMut(Stream, &str),
{
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let read = |stream: Stream,
                source: Box<dyn std::io::Read + Send>,
                tx: mpsc::Sender<(Stream, String)>| {
        std::thread::spawn(move || {
            for value in BufReader::new(source).lines().map_while(Result::ok) {
                if tx.send((stream, value)).is_err() {
                    break;
                }
            }
        })
    };
    let stdout = read(
        Stream::Stdout,
        Box::new(child.stdout.take().ok_or("Нет stdout")?),
        tx.clone(),
    );
    let stderr = read(
        Stream::Stderr,
        Box::new(child.stderr.take().ok_or("Нет stderr")?),
        tx,
    );
    loop {
        if cancel.load(Ordering::Acquire) {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                let mut kill = Command::new("taskkill");
                use std::os::windows::process::CommandExt;
                kill.creation_flags(0x08000000);
                let _ = kill
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill();
            let _ = child.wait();
            // Readers are allowed to finish asynchronously if an OS pipe is
            // held by a detached descendant; cancellation never waits on logs.
            return Ok(false);
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((stream, value)) => line(stream, value.trim()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_millis(50))
            }
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())?
            && stdout.is_finished()
            && stderr.is_finished()
        {
            let _ = stdout.join();
            let _ = stderr.join();
            for (stream, value) in rx.try_iter() {
                line(stream, value.trim());
            }
            return Ok(status.success());
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn cancels_silent_process_and_descendants() {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            trigger.store(true, Ordering::Release);
        });
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = std::time::Instant::now();
        assert!(!run(cmd, cancel, |_, _| {}).unwrap());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn remains_cancellable_when_exited_parent_leaves_open_pipes() {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            trigger.store(true, Ordering::Release);
        });
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 &"]);
        let started = std::time::Instant::now();
        assert!(!run(command, cancel, |_, _| {}).unwrap());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
