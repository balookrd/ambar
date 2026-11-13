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

    /// Empty marker that says the volume has a data source. Written by a
    /// later `apply` pass, never by the pass that ran the skip of the initial
    /// sync: only on a node with a copy, only in a pass that has nothing else
    /// to do, and only when the status matches the description and shows the
    /// local volume-0 disk `UpToDate`, or a peer's volume-0 disk `UpToDate`,
    /// `Consistent` or `Outdated` (a peer holds data, so this copy resyncs
    /// from it). While it is absent the volume counts as never synchronised
    /// on this node.
    pub fn synced_marker(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.img.synced"))
    }

    pub fn res_file(&self, volume: &str) -> PathBuf {
        self.res_dir.join(format!("{volume}.res"))
    }

    /// Copy of the resource file as last applied with a confirmed effect
    /// (`drbdadm up` or `adjust` followed by a matching status). Not matched
    /// by the `*.res` include of `drbd.conf`.
    pub fn applied_res(&self, volume: &str) -> PathBuf {
        self.res_dir.join(format!("{volume}.res.applied"))
    }

    /// Held with an exclusive advisory lock by `apply` and `remove`.
    pub fn lock_file(&self, volume: &str) -> PathBuf {
        self.volumes_dir.join(format!("{volume}.lock"))
    }

    pub fn device_link(&self, volume: &str) -> PathBuf {
        self.dev_dir.join(volume)
    }
}
