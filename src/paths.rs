//! Where the agent keeps volume files, DRBD resource files and device links.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Sparse backing files, one per volume with a copy on this node.
    pub volumes_dir: PathBuf,
    /// DRBD resource files; `/etc/drbd.conf` includes `*.res` from here.
    pub res_dir: PathBuf,
    /// Stable names for the loop devices: `<dev_dir>/<volume>` -> `/dev/loopN`.
    pub dev_dir: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            volumes_dir: PathBuf::from("/var/lib/rancher/ambar/volumes"),
            res_dir: PathBuf::from("/etc/drbd.d"),
            dev_dir: PathBuf::from("/dev/ambar"),
        }
    }
}

impl Paths {
    /// Sparse backing file of the copy on this node.
    pub fn image(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.img"))
    }

    /// Empty marker written after DRBD metadata was created on the backing
    /// file. A backing file without it has to be initialised again.
    pub fn ready_marker(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.img.ready"))
    }

    /// Empty marker written once the copy on this node has been in sync: after
    /// the leader skipped the initial sync, or when the local disk was first
    /// seen `UpToDate`. While it is absent the volume counts as never
    /// synchronised on this node.
    pub fn synced_marker(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.img.synced"))
    }

    pub fn res_file(&self, volume: &str) -> PathBuf {
        self.res_dir.join(format!("{volume}.res"))
    }

    pub fn device_link(&self, volume: &str) -> PathBuf {
        self.dev_dir.join(volume)
    }
}
