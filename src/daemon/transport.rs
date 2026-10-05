//! Platform IPC transport for the shared daemon.
//!
//! The daemon's wire protocol ([`super::protocol`]) is transport-agnostic — it
//! reads and writes length-prefixed frames over any `AsyncRead`/`AsyncWrite`.
//! This module supplies the byte stream underneath it, choosing the native
//! single-machine IPC primitive per platform so the daemon has the same
//! capability everywhere:
//!
//! - **Unix:** a Unix domain socket bound to the resolved socket path. A stale
//!   socket left by a crashed daemon is reclaimed atomically (bind a per-pid
//!   temp path, then `rename` over the target). The bind path is permission-
//!   hardened: the socket's parent directory (the daemon folder) is created
//!   with mode 0700, or verified to be owned by this uid with mode 0700 — a
//!   folder with another owner or a looser mode is refused, never silently
//!   used — and the socket file is chmod'd 0600, so other local users can
//!   neither traverse to nor connect to the socket even when it falls back to
//!   a shared location like `/tmp` (see [`super::default_daemon_dir`]). A
//!   third layer — an `SO_PEERCRED` uid check — lives in the server's accept
//!   loop.
//! - **Windows:** a named pipe whose name is derived from the same resolved
//!   path, so the `--socket` / `ENGRAMDB_DAEMON_SOCKET` / global
//!   `[daemon].socket_path` override chain (and the per-user default path) carry over unchanged. The
//!   pipe name is `\\.\pipe\engramdb-<hash>`; an explicit `\\.\pipe\...` value is
//!   used verbatim. Named pipes vanish when their owning process exits, so no
//!   stale-state reclamation is needed.
//!
//! Both `bind_or_yield` implementations share the same contract: `Ok(Some(_))`
//! means this process now owns the address, `Ok(None)` means a live daemon
//! already owns it (this process should yield), and `Err(_)` is a real failure.
//! Single-owner coordination is the binding primitive itself: on Unix only one
//! process can `bind` a path; on Windows only the *first* pipe instance may set
//! `first_pipe_instance`, so a second daemon's create fails while one is alive.

use std::path::Path;

#[cfg(unix)]
pub use unix::{bind_or_yield, connect, daemon_dir_problem, prepare_daemon_dir, Listener};

#[cfg(windows)]
pub use windows::{bind_or_yield, connect, daemon_dir_problem, prepare_daemon_dir, Listener};

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use tokio::net::{UnixListener, UnixStream};

    /// Mode for the socket's parent directory: owner-only, no group/other
    /// traversal.
    const DIR_MODE: u32 = 0o700;
    /// Mode for the socket file itself: owner read/write only.
    const SOCKET_MODE: u32 = 0o600;

    /// A bound Unix-domain-socket listener.
    pub struct Listener(UnixListener);

    impl Listener {
        /// Accept the next client connection.
        pub async fn accept(&self) -> std::io::Result<UnixStream> {
            let (stream, _addr) = self.0.accept().await?;
            Ok(stream)
        }
    }

    /// Create (or verify) the daemon folder with owner-only access.
    ///
    /// Defense layer 1 of the daemon's local-only access control. The folder
    /// holds the socket, the reclaim lock and the daemon log, and it can land
    /// outside `$XDG_RUNTIME_DIR` (the per-user cache dir, `/tmp/engramdb-<uid>`
    /// as a last resort, or an override), where a umask-default directory would
    /// be world-traversable.
    ///
    /// - **Missing:** created (recursively) with mode 0700.
    /// - **Exists, owned by us, mode 0700 (no group/other bits):** used.
    /// - **Anything else** — another owner, group/other bits set, or not a
    ///   directory: refused with an error that names the fix. The daemon does
    ///   not chmod a folder it finds: a loose folder that already exists may
    ///   be deliberately shared, and a folder someone else owns invites socket
    ///   squatting or swaps. (A socket placed *directly* in a root-owned dir
    ///   like `/tmp` is therefore refused; the default always uses an owned
    ///   subdirectory.)
    pub fn prepare_daemon_dir(dir: &Path) -> std::io::Result<()> {
        if dir.as_os_str().is_empty() {
            return Ok(());
        }
        // Recursive create succeeds (like `create_dir_all`) when the dir
        // already exists; the mode only applies to directories we create.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(dir)?;
        verify_daemon_dir(dir, crate::daemon::current_euid())
    }

    /// Why an *existing* daemon folder would be refused, or `None` when it is
    /// missing (it will be created private) or acceptable. Read-only: unlike
    /// [`prepare_daemon_dir`] it creates nothing, so `doctor` can call it.
    pub fn daemon_dir_problem(dir: &Path) -> Option<std::io::Error> {
        if !dir.exists() {
            return None;
        }
        verify_daemon_dir(dir, crate::daemon::current_euid()).err()
    }

    /// The ownership and mode check of [`prepare_daemon_dir`], split out so a
    /// test can drive the "owned by another uid" branch without root.
    pub(crate) fn verify_daemon_dir(dir: &Path, euid: u32) -> std::io::Result<()> {
        let meta = fs::metadata(dir)?;
        if !meta.is_dir() {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("daemon folder {} is not a directory", dir.display()),
            ));
        }
        if meta.uid() != euid {
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "daemon folder {} is owned by uid {} (we are uid {}); refusing to \
                     bind — point the global [daemon].socket_path (or \
                     ENGRAMDB_DAEMON_SOCKET) into a directory you own",
                    dir.display(),
                    meta.uid(),
                    euid
                ),
            ));
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "daemon folder {} has mode {mode:o}, but it must be private (700); \
                     refusing to bind — run `chmod 700 {}`, or point the socket into a \
                     private directory",
                    dir.display(),
                    dir.display()
                ),
            ));
        }
        Ok(())
    }

    /// Defense layer 2: restrict the socket file itself to owner read/write.
    /// A Unix socket inherits `0777 & ~umask` at bind, which would let any
    /// user who can traverse to it connect.
    fn restrict_socket(path: &Path) -> std::io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
    }

    /// Bind the socket, reclaiming a stale one left by a crashed daemon.
    ///
    /// Returns `Ok(None)` when a *live* daemon already owns the socket (this
    /// process should exit), `Ok(Some(listener))` when we own it. On success
    /// the parent directory is mode 0700 and the socket file mode 0600 (see
    /// [`prepare_daemon_dir`] / [`restrict_socket`]).
    pub async fn bind_or_yield(socket: &Path) -> std::io::Result<Option<Listener>> {
        // The socket is a real filesystem entry, so its directory must exist
        // before bind. (Windows named pipes have no parent directory, hence
        // this lives in the Unix transport rather than the shared server loop.)
        //
        // `daemon_dir_for` maps a bare relative name (`--socket d.sock`) to
        // `.`, so the current directory is checked like any other folder —
        // the same folder the log open checks — rather than skipped.
        prepare_daemon_dir(&crate::daemon::daemon_dir_for(socket))?;
        match UnixListener::bind(socket) {
            Ok(l) => {
                // Brief bind→chmod window with umask-default permissions; the
                // 0700 parent already blocks other users for its duration.
                restrict_socket(socket)?;
                return Ok(Some(Listener(l)));
            }
            Err(e) if e.kind() != ErrorKind::AddrInUse => return Err(e),
            Err(_) => {}
        }
        // Path is occupied. If something answers, a live daemon owns it.
        if UnixStream::connect(socket).await.is_ok() {
            return Ok(None);
        }
        // No listener — the socket file is stale. Reclaim it atomically: bind a
        // private per-pid path, then `rename` it over the target. `rename` is
        // atomic and replaces the entry in-place, so there's never a window
        // where the target has no listener.
        //
        // Reclaim itself is serialized by an advisory flock on a sibling lock
        // file: `rename(2)` REPLACES the target, so two processes that both
        // observed the socket stale would otherwise both bind+rename, the
        // second replacing the first's just-bound socket — orphaning a live
        // daemon that keeps its models resident (receiving no pings, winning
        // no clients) until its idle timeout. Under the lock, the loser
        // re-probes and yields to the winner instead. The lock is held only
        // for the probe+bind+rename instants; the lock *file* (not the
        // socket) is left behind, which is harmless.
        let reclaim_lock = socket.with_extension("reclaim.lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&reclaim_lock)?;
        // Bounded acquisition: a competing reclaimer holds this only for the
        // probe+bind+rename instants, so contention resolves in milliseconds.
        // If the holder is wedged (stopped mid-reclaim), proceeding WITHOUT
        // the lock after the deadline restores the pre-lock behavior (a
        // narrow orphan-a-daemon race) — strictly better than hanging every
        // future daemon startup behind a dead process's lock.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut locked = false;
        loop {
            match lock_file.try_lock() {
                Ok(()) => {
                    locked = true;
                    break;
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(e) => {
                    tracing::warn!(
                        "daemon socket reclaim lock unavailable after 2s ({e}); \
                         proceeding unlocked"
                    );
                    break;
                }
            }
        }
        let result = (|| {
            // Re-probe under the lock: if we lost the reclaim race, the
            // winner's daemon now answers on the target path.
            if std::os::unix::net::UnixStream::connect(socket).is_ok() {
                return Ok(None);
            }
            let tmp = socket.with_extension(format!("tmp.{}", std::process::id()));
            let _ = std::fs::remove_file(&tmp);
            let listener = UnixListener::bind(&tmp)?;
            // chmod the temp-path socket BEFORE renaming it over the target,
            // so the target path never exposes a group/world-accessible
            // socket, even for an instant.
            if let Err(e) = restrict_socket(&tmp).and_then(|()| std::fs::rename(&tmp, socket)) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            Ok(Some(Listener(listener)))
        })();
        if locked {
            let _ = lock_file.unlock();
        }
        result
    }

    /// Connect to the daemon's socket.
    pub async fn connect(socket: &Path) -> std::io::Result<UnixStream> {
        UnixStream::connect(socket).await
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    // `winerror.h` codes we branch on (avoiding a `windows-sys` dep here).
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_PIPE_BUSY: i32 = 231;

    /// Map the resolved socket path to a named-pipe name. An explicit pipe name
    /// is honored verbatim; any other path is hashed into a stable name so the
    /// override chain and per-user default path still produce distinct,
    /// reproducible pipes (the default path lives under the user's local app
    /// data, giving per-user isolation for free).
    fn pipe_name(socket: &Path) -> String {
        let s = socket.to_string_lossy();
        let lower = s.to_ascii_lowercase();
        if lower.starts_with(r"\\.\pipe\") || lower.starts_with(r"\\?\pipe\") {
            return s.into_owned();
        }
        let mut hasher = Sha256::new();
        hasher.update(s.as_bytes());
        let digest = hasher.finalize();
        let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!(r"\\.\pipe\engramdb-{hex}")
    }

    /// Create the daemon folder (it holds the log; a named pipe itself has no
    /// directory). Windows has no Unix mode bits: the folder defaults to the
    /// per-user local app data, whose ACL is already owner-only.
    pub fn prepare_daemon_dir(dir: &Path) -> std::io::Result<()> {
        if dir.as_os_str().is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(dir)
    }

    /// No mode bits to check on Windows; see [`prepare_daemon_dir`].
    pub fn daemon_dir_problem(_dir: &Path) -> Option<std::io::Error> {
        None
    }

    /// A bound named-pipe listener. Holds the next unconnected pipe instance;
    /// [`Listener::accept`] connects it and creates the replacement, following
    /// the documented Tokio named-pipe server loop.
    pub struct Listener {
        name: String,
        current: Mutex<Option<NamedPipeServer>>,
    }

    impl Listener {
        /// Accept the next client connection.
        ///
        /// Every early return must leave `current` repopulated: the daemon's
        /// accept loop treats an `Err` here as transient (`continue`), so a
        /// single failed `connect()`/`create()` that left `current` empty
        /// would make the NEXT call fail unrecoverably — the daemon would
        /// stop accepting until manually killed while still owning the pipe
        /// name.
        pub async fn accept(&self) -> std::io::Result<NamedPipeServer> {
            // Take the pending instance, creating a fresh one if a previous
            // error consumed it (held briefly, never across the await).
            let server = match self.current.lock().unwrap().take() {
                Some(s) => s,
                None => ServerOptions::new().create(&self.name)?,
            };
            if let Err(e) = server.connect().await {
                // Put the (still unconnected) instance back so the pipe name
                // keeps an owner and the next accept can retry.
                *self.current.lock().unwrap() = Some(server);
                return Err(e);
            }
            // Prepare the next instance so the pipe name keeps an owner. On
            // failure, return the connected client anyway — `current` stays
            // empty and the next accept re-creates it above.
            match ServerOptions::new().create(&self.name) {
                Ok(next) => *self.current.lock().unwrap() = Some(next),
                Err(e) => tracing::warn!(
                    "named-pipe listener could not pre-create the next instance \
                     (will retry on next accept): {e}"
                ),
            }
            Ok(server)
        }
    }

    /// Create the first pipe instance, yielding if a live daemon owns the name.
    ///
    /// Two independent checks, so the contract holds even where one mechanism
    /// is weak:
    /// 1. **Probe** — try to open the pipe as a client. If an instance is
    ///    listening (`Ok`) or exists but is busy (`ERROR_PIPE_BUSY`), a live
    ///    daemon owns the name → `Ok(None)` (yield). This mirrors the Unix
    ///    connect-probe and is what detects contention on runtimes that don't
    ///    enforce `FILE_FLAG_FIRST_PIPE_INSTANCE` (e.g. Wine). The probe
    ///    connection is dropped immediately; the owner sees a connect/disconnect
    ///    and replenishes its pending instance.
    /// 2. **`first_pipe_instance`** — the create still sets this flag, so on a
    ///    genuine startup race (both probes saw no pipe) exactly one `create`
    ///    wins and the loser gets `ERROR_ACCESS_DENIED` → `Ok(None)`.
    ///
    /// No stale state is possible: a crashed daemon's pipe instances are gone,
    /// so the probe fails with `ERROR_FILE_NOT_FOUND` and a fresh `create`
    /// succeeds.
    pub async fn bind_or_yield(socket: &Path) -> std::io::Result<Option<Listener>> {
        let name = pipe_name(socket);

        match ClientOptions::new().open(&name) {
            // A server instance is listening (or exists but busy): yield.
            Ok(_) => return Ok(None),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => return Ok(None),
            // No pipe (`ERROR_FILE_NOT_FOUND`) or any other probe error: fall
            // through and let `create` decide.
            Err(_) => {}
        }

        match ServerOptions::new().first_pipe_instance(true).create(&name) {
            Ok(server) => Ok(Some(Listener {
                name,
                current: Mutex::new(Some(server)),
            })),
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Connect to the daemon's named pipe.
    ///
    /// A server instance can be momentarily busy in the gap between `accept`
    /// returning and the replacement instance being created; retry briefly on
    /// `ERROR_PIPE_BUSY`, as the Tokio docs prescribe. A missing pipe (no
    /// daemon) surfaces as `ERROR_FILE_NOT_FOUND`, which the caller maps to
    /// "not running".
    pub async fn connect(socket: &Path) -> std::io::Result<NamedPipeClient> {
        let name = pipe_name(socket);
        for _ in 0..20 {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(client),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(e) => return Err(e),
            }
        }
        ClientOptions::new().open(&name)
    }
}

// Platform-equivalent behavior tests. These run on *every* platform: the test
// address is a temp path, which the Unix transport binds as a socket and the
// Windows transport maps to a uniquely-named pipe, so the same assertions cover
// both IPC primitives. The Unix-socket-specific scenario (a stale regular file
// left at the path) lives in `daemon::tests::daemon_reclaims_stale_socket`; its
// Windows-equivalent coverage is `bind_succeeds_after_owner_drops` below.
#[cfg(test)]
mod tests {
    use super::{bind_or_yield, connect};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A full client↔server byte round-trip over the platform IPC stream:
    /// bind, accept on a task, connect, write, echo, read back.
    #[tokio::test]
    async fn bind_then_connect_roundtrip() {
        let tmp = crate::daemon::private_tempdir();
        let addr = tmp.path().join("rt.sock");

        let listener = bind_or_yield(&addr)
            .await
            .unwrap()
            .expect("first bind owns the address");
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping");
            stream.write_all(b"pong").await.unwrap();
            stream.flush().await.unwrap();
        });

        let mut client = connect(&addr).await.expect("client connects");
        client.write_all(b"ping").await.unwrap();
        client.flush().await.unwrap();
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
        server.await.unwrap();
    }

    /// While one process owns the address, a second bind must yield (so only
    /// one daemon ever serves a given socket/pipe).
    #[tokio::test]
    async fn second_bind_yields_to_live_owner() {
        let tmp = crate::daemon::private_tempdir();
        let addr = tmp.path().join("yield.sock");

        let _owner = bind_or_yield(&addr)
            .await
            .unwrap()
            .expect("first bind owns the address");
        let second = bind_or_yield(&addr).await.unwrap();
        assert!(
            second.is_none(),
            "a second bind must yield to the live owner"
        );
    }

    /// After the owner goes away (crashed daemon), a fresh bind succeeds: the
    /// Unix transport reclaims the stale socket file, the Windows transport
    /// finds the pipe name free. This is the cross-platform analog of stale-
    /// socket reclamation.
    #[tokio::test]
    async fn bind_succeeds_after_owner_drops() {
        let tmp = crate::daemon::private_tempdir();
        let addr = tmp.path().join("reclaim.sock");

        let owner = bind_or_yield(&addr)
            .await
            .unwrap()
            .expect("first bind owns the address");
        assert!(
            bind_or_yield(&addr).await.unwrap().is_none(),
            "yields while the owner is alive"
        );
        drop(owner);
        assert!(
            bind_or_yield(&addr).await.unwrap().is_some(),
            "a new daemon reclaims the address after the owner drops"
        );
    }

    /// Connecting with no listener present is an error (mapped by callers to
    /// "daemon not running"), never a hang.
    #[tokio::test]
    async fn connect_to_missing_address_fails() {
        let tmp = crate::daemon::private_tempdir();
        let addr = tmp.path().join("absent.sock");
        assert!(
            connect(&addr).await.is_err(),
            "connecting with no listener must fail"
        );
    }

    // Unix-only permission hardening: these assert on real filesystem modes,
    // which only exist for the Unix-domain-socket transport (Windows named
    // pipes have no on-disk representation).
    #[cfg(unix)]
    mod unix_permissions {
        use super::super::bind_or_yield;
        use std::os::unix::fs::PermissionsExt;

        fn mode_of(path: &std::path::Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        /// A fresh bind creates the socket's parent directory with mode 0700
        /// and the socket file with mode 0600 — owner-only at both layers.
        #[tokio::test]
        async fn bind_sets_owner_only_permissions() {
            let tmp = crate::daemon::private_tempdir();
            // Parent does not exist yet: bind must create it 0700.
            let parent = tmp.path().join("rt").join("engramdb");
            let addr = parent.join("perm.sock");

            let _l = bind_or_yield(&addr)
                .await
                .unwrap()
                .expect("first bind owns the address");
            assert_eq!(mode_of(&parent), 0o700, "socket dir must be 0700");
            assert_eq!(mode_of(&addr), 0o600, "socket file must be 0600");
        }

        /// Reclaiming a stale socket (the bind-temp-then-rename path) must
        /// also end with a 0600 socket — and the chmod happens on the temp
        /// path *before* the rename, so the target is never world-accessible.
        #[tokio::test]
        async fn stale_reclaim_sets_owner_only_socket() {
            let tmp = crate::daemon::private_tempdir();
            let addr = tmp.path().join("stale.sock");
            // A dead socket file with permissive mode, as a crashed pre-
            // hardening daemon could have left behind.
            std::fs::write(&addr, b"stale - not a live socket").unwrap();
            std::fs::set_permissions(&addr, std::fs::Permissions::from_mode(0o666)).unwrap();

            let _l = bind_or_yield(&addr)
                .await
                .unwrap()
                .expect("stale socket must be reclaimed");
            assert_eq!(mode_of(&addr), 0o600, "reclaimed socket must be 0600");
        }

        /// A pre-existing daemon folder we own but with group/other access is
        /// refused, not tightened: the daemon never chmods a folder it did not
        /// create, and never binds in one that is not private.
        #[tokio::test]
        async fn loose_existing_dir_is_refused() {
            let tmp = crate::daemon::private_tempdir();
            let parent = tmp.path().join("loose");
            std::fs::create_dir(&parent).unwrap();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();

            let addr = parent.join("d.sock");
            let err = match bind_or_yield(&addr).await {
                Err(e) => e,
                Ok(_) => panic!("a 0755 daemon folder must be refused"),
            };
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(err.to_string().contains("chmod 700"), "{err}");
            assert_eq!(mode_of(&parent), 0o755, "the folder must not be changed");
            assert!(!addr.exists(), "nothing may be bound in a refused folder");
        }

        /// Group-only access is just as refused as world access.
        #[tokio::test]
        async fn group_accessible_dir_is_refused() {
            let tmp = crate::daemon::private_tempdir();
            let parent = tmp.path().join("group");
            std::fs::create_dir(&parent).unwrap();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o710)).unwrap();
            assert!(bind_or_yield(&parent.join("d.sock")).await.is_err());
        }

        /// A folder owned by another uid is refused. Driven through the check
        /// itself with a uid that is not ours, because a test cannot chown a
        /// directory to someone else without root.
        #[test]
        fn foreign_owned_dir_is_refused() {
            let tmp = crate::daemon::private_tempdir();
            let me = crate::daemon::current_euid();
            assert!(super::super::unix::verify_daemon_dir(tmp.path(), me).is_ok());
            let err = super::super::unix::verify_daemon_dir(tmp.path(), me.wrapping_add(1))
                .expect_err("a folder owned by another uid must be refused");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(err.to_string().contains("owned by uid"), "{err}");
        }

        /// A bare relative socket name lives in the current directory, and
        /// that directory gets the same check as any other daemon folder (it
        /// used to be skipped because the path's parent is empty).
        #[tokio::test]
        async fn relative_socket_checks_the_current_directory() {
            let tmp = tempfile::TempDir::new().unwrap();
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            // Process-global, but nextest runs each test in its own process.
            std::env::set_current_dir(tmp.path()).unwrap();
            assert!(
                bind_or_yield(std::path::Path::new("rel.sock"))
                    .await
                    .is_err(),
                "a relative socket in a 0755 cwd must be refused"
            );
            assert!(!tmp.path().join("rel.sock").exists());
            assert!(super::super::daemon_dir_problem(std::path::Path::new(".")).is_some());

            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(super::super::daemon_dir_problem(std::path::Path::new(".")).is_none());
            assert!(bind_or_yield(std::path::Path::new("rel.sock"))
                .await
                .unwrap()
                .is_some());
        }

        /// A file where the folder should be is refused, not bound beside.
        #[test]
        fn non_directory_is_refused() {
            let tmp = crate::daemon::private_tempdir();
            let file = tmp.path().join("f");
            std::fs::write(&file, b"x").unwrap();
            assert!(super::super::prepare_daemon_dir(&file).is_err());
        }
    }
}
