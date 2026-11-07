//! Detect a path that lies on a volume which is not mounted right now.
//!
//! A project on a removable drive (a USB stick, an external disk, an SD
//! card) or a network share disappears from the filesystem while the volume
//! is not mounted. On macOS `/Volumes/<name>` is removed on eject, and on
//! Linux udisks removes `/media/<user>/<label>` and `/run/media/<user>/<label>`.
//! So `try_exists` on such a project returns `Ok(false)`, not an error: from
//! the filesystem's view the project was deleted.
//!
//! The registry cleanup (`ops::projects::prune_stale_projects`) runs
//! unattended from auto-maintenance in *any* project. Without this check it
//! dropped the registry entry of every project on an unplugged drive, together
//! with that entry's group subscriptions and parent link, and the project then
//! looked unregistered the next time the drive was plugged in.
//!
//! The check is deliberately narrow. A path counts as "on an absent volume"
//! only when the missing part of the path starts in a well-known mount
//! container or in a direct child of one, and that child is not itself a
//! mount point:
//!
//! - `/Volumes/Drive/proj`, with `/Volumes/Drive` missing: absent volume.
//! - `/Volumes/Drive/proj`, with `/Volumes/Drive` mounted and `proj` deleted:
//!   the deepest existing directory is `/Volumes/Drive`, a mount point, so the
//!   project is really gone.
//! - `/home/me/proj` deleted: `/home/me` is not a mount container, so the
//!   project is really gone.
//!
//! On Windows, a path whose drive root (`E:\`) does not exist is on an absent
//! volume.
//!
//! A missing directory directly in a top-level container (`/Volumes`,
//! `/media`, `/run/media`, `/mnt`) always counts as an absent volume, even
//! when the container is itself a mount point, as `/mnt` often is.
//!
//! The function errs toward "absent volume" for an existing, non-mounted
//! directory directly under a container (for example a plain folder
//! `/mnt/data`). The cost of that is one registry entry that the automatic
//! cleanup keeps; the cost of the other direction is a lost registration.

use std::io;
use std::path::{Path, PathBuf};

/// Directories whose children are mount points of removable or network
/// volumes.
///
/// A child of one of these (for example `/media/<user>`) is a container too,
/// because udisks mounts at `/media/<user>/<label>` and
/// `/run/media/<user>/<label>`, and it can remove the `<user>` directory when
/// nothing is mounted.
#[cfg(unix)]
const MOUNT_CONTAINERS: &[&str] = &["/Volumes", "/media", "/run/media", "/mnt"];

/// Whether `path` lies on a removable or network volume that is not mounted
/// now.
///
/// Returns `false` when the path exists, when it cannot be told apart from a
/// deleted directory, and on any probe error other than "not found".
pub fn on_unmounted_volume(path: &Path) -> bool {
    on_unmounted_volume_with(path, &RealFs)
}

/// The filesystem probes [`on_unmounted_volume`] needs. A trait so the
/// decision can be tested without real mount points.
pub(crate) trait VolumeProbe {
    /// `Path::try_exists`.
    fn exists(&self, path: &Path) -> io::Result<bool>;
    /// Whether `path` is the root of a mounted filesystem.
    fn is_mount_point(&self, path: &Path) -> bool;
}

struct RealFs;

impl VolumeProbe for RealFs {
    fn exists(&self, path: &Path) -> io::Result<bool> {
        path.try_exists()
    }

    #[cfg(unix)]
    fn is_mount_point(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Some(parent) = path.parent() else {
            // The filesystem root is always a mount point.
            return true;
        };
        match (std::fs::metadata(path), std::fs::metadata(parent)) {
            (Ok(own), Ok(up)) => own.dev() != up.dev(),
            // Cannot tell. "Not a mount point" makes the caller keep the
            // registry entry, which is the safe direction.
            _ => false,
        }
    }

    #[cfg(not(unix))]
    fn is_mount_point(&self, _path: &Path) -> bool {
        false
    }
}

pub(crate) fn on_unmounted_volume_with(path: &Path, fs: &dyn VolumeProbe) -> bool {
    #[cfg(windows)]
    {
        if let Some(root) = windows_volume_root(path) {
            return matches!(fs.exists(&root), Ok(false));
        }
        false
    }
    #[cfg(not(windows))]
    {
        unix_on_unmounted_volume(path, fs)
    }
}

#[cfg(not(windows))]
fn unix_on_unmounted_volume(path: &Path, fs: &dyn VolumeProbe) -> bool {
    if !path.is_absolute() {
        return false;
    }
    // The deepest ancestor (or the path itself) that exists.
    let mut deepest: Option<PathBuf> = None;
    for ancestor in path.ancestors() {
        match fs.exists(ancestor) {
            Ok(true) => {
                deepest = Some(ancestor.to_path_buf());
                break;
            }
            Ok(false) => continue,
            // EACCES, ESTALE, a hung mount: unknown, not "absent volume".
            Err(_) => return false,
        }
    }
    let Some(deepest) = deepest else {
        return false;
    };
    if deepest == path {
        return false;
    }
    match container_level(&deepest) {
        // The missing directory is the mount itself (`/Volumes/<name>`) or
        // the per-user level udisks removes (`/media/<user>`). The container
        // can be a mount point of its own (`/mnt` often is), which says
        // nothing about its children.
        Some(ContainerLevel::Top) => true,
        // `/media/<user>` or `/media/<label>`: a mounted volume whose
        // project directory was deleted, unless it is a plain directory.
        Some(ContainerLevel::Child) => !fs.is_mount_point(&deepest),
        None => false,
    }
}

#[cfg(not(windows))]
enum ContainerLevel {
    /// One of [`MOUNT_CONTAINERS`].
    Top,
    /// A direct child of one.
    Child,
}

/// Whether `dir` is one of [`MOUNT_CONTAINERS`] or a direct child of one.
#[cfg(not(windows))]
fn container_level(dir: &Path) -> Option<ContainerLevel> {
    #[cfg(unix)]
    let containers = MOUNT_CONTAINERS;
    #[cfg(not(unix))]
    let containers: &[&str] = &[];
    containers.iter().find_map(|c| {
        let c = Path::new(c);
        if dir == c {
            Some(ContainerLevel::Top)
        } else if dir.parent() == Some(c) {
            Some(ContainerLevel::Child)
        } else {
            None
        }
    })
}

/// The drive root (`E:\`, `\\server\share\`) of a Windows path, if it has one.
#[cfg(windows)]
fn windows_volume_root(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut components = path.components();
    match components.next()? {
        Component::Prefix(prefix) => {
            let mut root = PathBuf::from(prefix.as_os_str());
            root.push(std::path::MAIN_SEPARATOR_STR);
            Some(root)
        }
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A fake filesystem: the set of directories that exist, and the subset of
    /// them that are mount points.
    struct FakeFs {
        dirs: HashSet<PathBuf>,
        mounts: HashSet<PathBuf>,
    }

    impl FakeFs {
        fn new(dirs: &[&str], mounts: &[&str]) -> Self {
            Self {
                dirs: dirs.iter().map(PathBuf::from).collect(),
                mounts: mounts.iter().map(PathBuf::from).collect(),
            }
        }
    }

    impl VolumeProbe for FakeFs {
        fn exists(&self, path: &Path) -> io::Result<bool> {
            Ok(path == Path::new("/") || self.dirs.contains(path))
        }
        fn is_mount_point(&self, path: &Path) -> bool {
            path == Path::new("/") || self.mounts.contains(path)
        }
    }

    fn check(fs: &FakeFs, path: &str) -> bool {
        on_unmounted_volume_with(Path::new(path), fs)
    }

    #[test]
    fn ejected_macos_volume_is_absent() {
        let fs = FakeFs::new(&["/Volumes"], &[]);
        assert!(check(&fs, "/Volumes/Drive/proj"));
    }

    #[test]
    fn deleted_project_on_mounted_macos_volume_is_gone() {
        let fs = FakeFs::new(&["/Volumes", "/Volumes/Drive"], &["/Volumes/Drive"]);
        assert!(!check(&fs, "/Volumes/Drive/proj"));
    }

    #[test]
    fn existing_project_is_not_on_an_absent_volume() {
        let fs = FakeFs::new(
            &["/Volumes", "/Volumes/Drive", "/Volumes/Drive/proj"],
            &["/Volumes/Drive"],
        );
        assert!(!check(&fs, "/Volumes/Drive/proj"));
    }

    #[test]
    fn a_container_that_is_itself_a_mount_point_still_counts() {
        let fs = FakeFs::new(&["/mnt"], &["/mnt"]);
        assert!(check(&fs, "/mnt/usb/proj"));
    }

    #[test]
    fn unmounted_udisks_volume_is_absent() {
        // `/media/<user>` exists, the label directory was removed on unmount.
        let fs = FakeFs::new(&["/media", "/media/me"], &[]);
        assert!(check(&fs, "/media/me/USB/work/proj"));
        // udisks can also remove the `<user>` directory itself.
        let fs = FakeFs::new(&["/run", "/run/media"], &[]);
        assert!(check(&fs, "/run/media/me/USB/proj"));
    }

    #[test]
    fn deleted_project_on_mounted_udisks_volume_is_gone() {
        let fs = FakeFs::new(
            &["/media", "/media/me", "/media/me/USB"],
            &["/media/me/USB"],
        );
        assert!(!check(&fs, "/media/me/USB/proj"));
    }

    #[test]
    fn deleted_project_on_the_system_disk_is_gone() {
        let fs = FakeFs::new(&["/home", "/home/me"], &[]);
        assert!(!check(&fs, "/home/me/proj"));
        // Deeper inside a container's subtree, the missing part does not
        // start in a container.
        let fs = FakeFs::new(&["/Volumes", "/Volumes/Drive", "/Volumes/Drive/a"], &[]);
        assert!(!check(&fs, "/Volumes/Drive/a/proj"));
    }

    #[test]
    fn relative_paths_are_never_on_an_absent_volume() {
        let fs = FakeFs::new(&[], &[]);
        assert!(!check(&fs, "Volumes/Drive/proj"));
    }

    #[test]
    fn a_probe_error_is_not_an_absent_volume() {
        struct Failing;
        impl VolumeProbe for Failing {
            fn exists(&self, _: &Path) -> io::Result<bool> {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
            fn is_mount_point(&self, _: &Path) -> bool {
                false
            }
        }
        assert!(!on_unmounted_volume_with(
            Path::new("/Volumes/Drive/proj"),
            &Failing
        ));
    }

    #[test]
    fn the_real_filesystem_root_is_never_an_absent_volume() {
        assert!(!on_unmounted_volume(Path::new("/")));
        // A temp directory exists, so it is not on an absent volume.
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!on_unmounted_volume(tmp.path()));
    }
}
