//! Safe startup for filesystem Unix-domain sockets.

use std::fs;
use std::fs::File;
use std::io;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;

fn lock_path(socket_path: &Path) -> PathBuf {
    let mut lock_path = socket_path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

/// Binds a nonblocking Unix listener without replacing an active peer.
///
/// Startup is serialized with an advisory lock beside `socket_path`. An
/// existing socket is removed only after a connection attempt is refused,
/// which identifies the path as stale. If a peer accepts a connection, this
/// returns [`io::ErrorKind::AddrInUse`] and leaves that peer's pathname
/// untouched. The returned listener is nonblocking and can be converted with
/// `tokio::net::UnixListener::from_std`.
///
/// # Errors
///
/// Returns an error when the parent directory cannot be accessed, another
/// server is active at `socket_path`, an occupied path cannot be proven stale,
/// or the replacement bind fails.
pub fn bind_unix_listener(socket_path: &Path) -> io::Result<UnixListener> {
    let lock_path = lock_path(socket_path);
    let startup_lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    startup_lock.lock()?;

    let listener = match UnixListener::bind(socket_path) {
        Ok(listener) => listener,
        Err(bind_error) if bind_error.kind() == io::ErrorKind::AddrInUse => {
            recover_or_refuse_active_socket(socket_path)?
        }
        Err(bind_error) => return Err(bind_error),
    };
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn recover_or_refuse_active_socket(socket_path: &Path) -> io::Result<UnixListener> {
    match UnixStream::connect(socket_path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "refusing to replace an active Unix socket at {}",
                socket_path.display()
            ),
        )),
        Err(connect_error) if connect_error.kind() == io::ErrorKind::ConnectionRefused => {
            fs::remove_file(socket_path)?;
            UnixListener::bind(socket_path)
        }
        // The path disappeared between bind and connect. Retrying the bind is
        // safe because no pathname remains to unlink.
        Err(connect_error) if connect_error.kind() == io::ErrorKind::NotFound => {
            UnixListener::bind(socket_path)
        }
        Err(connect_error) => Err(io::Error::new(
            connect_error.kind(),
            format!(
                "cannot determine whether Unix socket at {} is stale: {connect_error}",
                socket_path.display()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::thread;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn refuses_to_replace_an_active_socket() {
        let dir = TempDir::new().expect("create temp dir");
        let socket_path = dir.path().join("plugin.sock");
        let _active = bind_unix_listener(&socket_path).expect("bind active listener");

        let error = bind_unix_listener(&socket_path).expect_err("must refuse active listener");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        UnixStream::connect(&socket_path).expect("active listener pathname remains reachable");
    }

    #[test]
    fn recovers_a_stale_socket() {
        let dir = TempDir::new().expect("create temp dir");
        let socket_path = dir.path().join("plugin.sock");
        let stale = UnixListener::bind(&socket_path).expect("create stale listener");
        drop(stale);

        let recovered = bind_unix_listener(&socket_path).expect("recover stale listener");
        UnixStream::connect(&socket_path).expect("recovered listener accepts connections");
        drop(recovered);
    }

    #[test]
    fn concurrent_startups_leave_exactly_one_owner() {
        let dir = TempDir::new().expect("create temp dir");
        let socket_path = dir.path().join("plugin.sock");
        let barrier = Arc::new(Barrier::new(3));
        let (result_sender, result_receiver) = mpsc::channel();
        let (release_first, wait_first) = mpsc::channel();
        let (release_second, wait_second) = mpsc::channel();

        let handles = [wait_first, wait_second].map(|release_receiver| {
            let barrier = Arc::clone(&barrier);
            let socket_path = socket_path.clone();
            let result_sender = result_sender.clone();

            thread::spawn(move || {
                barrier.wait();
                match bind_unix_listener(&socket_path) {
                    Ok(listener) => {
                        result_sender
                            .send(Ok(()))
                            .expect("report successful startup");
                        release_receiver.recv().expect("release listener owner");
                        drop(listener);
                    }
                    Err(error) => result_sender
                        .send(Err(error.kind()))
                        .expect("report failed startup"),
                }
            })
        });
        drop(result_sender);

        barrier.wait();
        let outcomes = [
            result_receiver
                .recv()
                .expect("receive first startup result"),
            result_receiver
                .recv()
                .expect("receive second startup result"),
        ];

        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Err(io::ErrorKind::AddrInUse)))
                .count(),
            1
        );

        let _ = release_first.send(());
        let _ = release_second.send(());
        for handle in handles {
            handle.join().expect("startup candidate did not panic");
        }
    }
}
