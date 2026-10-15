use crate::{model::*, store::Store};
use anyhow::{Result, ensure};
use fuser::*;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TTL: Duration = Duration::ZERO;

pub struct WitnessMount {
    store: Store,
    branch: String,
    labels: HashMap<u64, PathBuf>,
    directories: HashMap<u64, Vec<(u64, FileType, OsString)>>,
    next_handle: u64,
}

fn file_type(kind: Kind) -> FileType {
    match kind {
        Kind::File => FileType::RegularFile,
        Kind::Directory => FileType::Directory,
        Kind::Symlink => FileType::Symlink,
    }
}
fn attributes(n: &Node) -> FileAttr {
    FileAttr {
        ino: n.id,
        size: n.size,
        blocks: n.chunks.len() as u64 * CHUNK_SIZE / 512,
        atime: UNIX_EPOCH + Duration::from_nanos(n.atime_ns),
        mtime: UNIX_EPOCH + Duration::from_nanos(n.mtime_ns),
        ctime: UNIX_EPOCH + Duration::from_nanos(n.ctime_ns),
        crtime: UNIX_EPOCH,
        kind: file_type(n.kind),
        perm: n.mode as u16,
        nlink: n.nlink,
        uid: n.uid,
        gid: n.gid,
        rdev: 0,
        blksize: CHUNK_SIZE as u32,
        flags: 0,
    }
}
fn errno(err: anyhow::Error) -> i32 {
    if let Some(e) = err.downcast_ref::<FsError>() {
        return e.0;
    }
    if let Some(e) = err.downcast_ref::<std::io::Error>() {
        return e.raw_os_error().unwrap_or(libc::EIO);
    }
    eprintln!("witnessfs: {err:#}");
    libc::EIO
}

impl WitnessMount {
    pub fn new(path: &Path, branch: &str) -> Result<Self> {
        let mut store = Store::open(path)?;
        store.hold_branch(branch)?;
        Ok(Self {
            store,
            branch: branch.into(),
            labels: HashMap::from([(ROOT, PathBuf::from("/"))]),
            directories: HashMap::new(),
            next_handle: 1,
        })
    }
    fn label(&self, ino: u64) -> String {
        self.labels
            .get(&ino)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("inode:{ino}"))
    }
    fn child_label(&self, parent: u64, name: &OsStr) -> String {
        Path::new(&self.label(parent))
            .join(name)
            .to_string_lossy()
            .into_owned()
    }
    fn remember(&mut self, node: &Node, path: &str) {
        self.labels.insert(node.id, PathBuf::from(path));
    }
    fn empty(reply: ReplyEmpty, result: Result<()>) {
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
}

impl Filesystem for WitnessMount {
    fn init(&mut self, _: &Request<'_>, config: &mut KernelConfig) -> std::result::Result<(), i32> {
        let _ = config.set_max_write(CHUNK_SIZE as u32);
        Ok(())
    }
    fn lookup(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let path = self.child_label(parent, name);
        let result = self
            .store
            .transact(&self.branch, |v| v.lookup(parent, name.as_bytes(), &path));
        match result {
            Ok(Some(n)) => {
                self.remember(&n, &path);
                reply.entry(&TTL, &attributes(&n), 0)
            }
            Ok(None) => reply.error(libc::ENOENT),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn getattr(&mut self, _: &Request<'_>, ino: u64, _: Option<u64>, reply: ReplyAttr) {
        let path = self.label(ino);
        match self.store.transact(&self.branch, |v| v.stat(ino, &path)) {
            Ok(n) => reply.attr(&TTL, &attributes(&n)),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn setattr(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _: Option<SystemTime>,
        _: Option<u64>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if flags.is_some_and(|f| f != 0) {
            reply.error(libc::EOPNOTSUPP);
            return;
        }
        let path = self.label(ino);
        let result = self.store.transact(&self.branch, |v| {
            let mut n = if let Some(size) = size {
                v.truncate(ino, size, &path)?
            } else {
                v.stat(ino, &path)?
            };
            if let Some(mode) = mode {
                n.mode = mode & 0o7777;
            }
            if let Some(uid) = uid {
                n.uid = uid;
            }
            if let Some(gid) = gid {
                n.gid = gid;
            }
            if let Some(mtime) = mtime {
                n.mtime_ns = match mtime {
                    TimeOrNow::Now => now_ns(),
                    TimeOrNow::SpecificTime(t) => {
                        t.duration_since(UNIX_EPOCH)
                            .map_err(|_| FsError(libc::EINVAL, "pre-epoch timestamps unsupported"))?
                            .as_nanos()
                            .min(u64::MAX as u128) as u64
                    }
                };
            }
            if let Some(atime) = atime {
                n.atime_ns = match atime {
                    TimeOrNow::Now => now_ns(),
                    TimeOrNow::SpecificTime(t) => {
                        t.duration_since(UNIX_EPOCH)
                            .map_err(|_| FsError(libc::EINVAL, "pre-epoch timestamps unsupported"))?
                            .as_nanos()
                            .min(u64::MAX as u128) as u64
                    }
                };
            }
            n.ctime_ns = now_ns();
            v.set_node(n.clone(), &path)?;
            Ok(n)
        });
        match result {
            Ok(n) => reply.attr(&TTL, &attributes(&n)),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn open(&mut self, _: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let path = self.label(ino);
        let result = self.store.transact(&self.branch, |v| {
            let n = if flags & libc::O_ACCMODE != libc::O_WRONLY {
                v.open_content(ino, &path)?
            } else {
                v.stat(ino, &path)?
            };
            require(n.kind == Kind::File, libc::EISDIR, "not a file")?;
            Ok(())
        });
        // Observe the whole content on every readable open. Cached reads and mmap
        // are then covered even when the kernel does not issue a FUSE read request.
        match result {
            Ok(()) => reply.opened(ino, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn read(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        _: u64,
        offset: i64,
        size: u32,
        _: i32,
        _: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let path = self.label(ino);
        match self.store.transact(&self.branch, |v| {
            v.read(ino, offset as u64, size as u64, &path)
        }) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn write(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        _: u64,
        offset: i64,
        data: &[u8],
        _: u32,
        flags: i32,
        _: Option<u64>,
        reply: ReplyWrite,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let path = self.label(ino);
        let result = self.store.transact(&self.branch, |v| {
            let offset = if flags & libc::O_APPEND != 0 {
                v.node(ino)?.size
            } else {
                offset as u64
            };
            v.write(ino, offset, data, &path)
        });
        match result {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        _: i32,
        reply: ReplyCreate,
    ) {
        let path = self.child_label(parent, name);
        match self.store.transact(&self.branch, |v| {
            v.create(
                parent,
                name.as_bytes(),
                Kind::File,
                mode & !umask,
                req.uid(),
                req.gid(),
                vec![],
                &path,
            )
        }) {
            Ok(n) => {
                self.remember(&n, &path);
                reply.created(&TTL, &attributes(&n), 0, n.id, 0)
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn mkdir(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let path = self.child_label(parent, name);
        match self.store.transact(&self.branch, |v| {
            v.create(
                parent,
                name.as_bytes(),
                Kind::Directory,
                mode & !umask,
                req.uid(),
                req.gid(),
                vec![],
                &path,
            )
        }) {
            Ok(n) => {
                self.remember(&n, &path);
                reply.entry(&TTL, &attributes(&n), 0)
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn mknod(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        _: u32,
        reply: ReplyEntry,
    ) {
        if mode & libc::S_IFMT != libc::S_IFREG {
            reply.error(libc::EOPNOTSUPP);
            return;
        }
        let path = self.child_label(parent, name);
        match self.store.transact(&self.branch, |v| {
            v.create(
                parent,
                name.as_bytes(),
                Kind::File,
                mode & !umask,
                req.uid(),
                req.gid(),
                vec![],
                &path,
            )
        }) {
            Ok(n) => {
                self.remember(&n, &path);
                reply.entry(&TTL, &attributes(&n), 0)
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn unlink(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let path = self.child_label(parent, name);
        Self::empty(
            reply,
            self.store.transact(&self.branch, |v| {
                v.unlink(parent, name.as_bytes(), false, &path)
            }),
        );
    }
    fn rmdir(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let path = self.child_label(parent, name);
        Self::empty(
            reply,
            self.store.transact(&self.branch, |v| {
                v.unlink(parent, name.as_bytes(), true, &path)
            }),
        );
    }
    fn symlink(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let path = self.child_label(parent, name);
        match self.store.transact(&self.branch, |v| {
            v.create(
                parent,
                name.as_bytes(),
                Kind::Symlink,
                0o777,
                req.uid(),
                req.gid(),
                target.as_os_str().as_bytes().to_vec(),
                &path,
            )
        }) {
            Ok(n) => {
                self.remember(&n, &path);
                reply.entry(&TTL, &attributes(&n), 0)
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn readlink(&mut self, _: &Request<'_>, ino: u64, reply: ReplyData) {
        let path = self.label(ino);
        match self
            .store
            .transact(&self.branch, |v| v.readlink(ino, &path))
        {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn link(&mut self, _: &Request<'_>, ino: u64, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let path = self.child_label(parent, name);
        match self.store.transact(&self.branch, |v| {
            v.link(ino, parent, name.as_bytes(), &path)
        }) {
            Ok(n) => reply.entry(&TTL, &attributes(&n), 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn rename(
        &mut self,
        _: &Request<'_>,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        if flags & !libc::RENAME_NOREPLACE != 0 {
            reply.error(libc::EOPNOTSUPP);
            return;
        }
        let path = self.child_label(parent, name);
        let new_path = self.child_label(new_parent, new_name);
        Self::empty(
            reply,
            self.store.transact(&self.branch, |v| {
                v.rename(
                    parent,
                    name.as_bytes(),
                    new_parent,
                    new_name.as_bytes(),
                    flags != 0,
                    &path,
                    &new_path,
                )
            }),
        );
    }
    fn opendir(&mut self, _: &Request<'_>, ino: u64, _: i32, reply: ReplyOpen) {
        let path = self.label(ino);
        // Capture a stable directory stream. Membership observations are conservative:
        // even an opened but unread directory becomes a dependency.
        let result = self.store.transact(&self.branch, |v| {
            Ok((v.stat(ino, &path)?, v.children(ino, &path)?))
        });
        match result {
            Ok((node, children)) => {
                let mut entries = vec![
                    (ino, FileType::Directory, OsString::from(".")),
                    (node.parent, FileType::Directory, OsString::from("..")),
                ];
                for (name, n) in children {
                    entries.push((n.id, file_type(n.kind), OsString::from_vec(name)));
                }
                let fh = self.next_handle;
                self.next_handle += 1;
                self.directories.insert(fh, entries);
                reply.opened(fh, 0);
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn readdir(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(entries) = self.directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*ino, (index + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }
    fn releasedir(&mut self, _: &Request<'_>, _: u64, fh: u64, _: i32, reply: ReplyEmpty) {
        self.directories.remove(&fh);
        reply.ok();
    }
    fn flush(&mut self, _: &Request<'_>, _: u64, _: u64, _: u64, reply: ReplyEmpty) {
        reply.ok();
    }
    fn fsync(&mut self, _: &Request<'_>, _: u64, _: u64, _: bool, reply: ReplyEmpty) {
        reply.ok();
    }
    fn fsyncdir(&mut self, _: &Request<'_>, _: u64, _: u64, _: bool, reply: ReplyEmpty) {
        reply.ok();
    }
    fn getxattr(&mut self, _: &Request<'_>, _: u64, _: &OsStr, _: u32, reply: ReplyXattr) {
        reply.error(libc::ENODATA);
    }
    fn listxattr(&mut self, _: &Request<'_>, _: u64, size: u32, reply: ReplyXattr) {
        if size == 0 {
            reply.size(0)
        } else {
            reply.data(&[])
        }
    }
    fn setxattr(
        &mut self,
        _: &Request<'_>,
        _: u64,
        _: &OsStr,
        _: &[u8],
        _: i32,
        _: u32,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::EOPNOTSUPP);
    }
    fn removexattr(&mut self, _: &Request<'_>, _: u64, _: &OsStr, reply: ReplyEmpty) {
        reply.error(libc::EOPNOTSUPP);
    }
}

pub fn options() -> Vec<MountOption> {
    vec![
        MountOption::FSName("witnessfs".into()),
        MountOption::DefaultPermissions,
        MountOption::NoSuid,
        MountOption::NoDev,
        MountOption::NoAtime,
    ]
}

pub fn foreground(store: &Path, branch: &str, mountpoint: &Path) -> Result<()> {
    ensure!(mountpoint.is_dir(), "mountpoint must exist");
    ensure!(
        std::fs::read_dir(mountpoint)?.next().is_none(),
        "mountpoint must be empty"
    );
    fuser::mount2(WitnessMount::new(store, branch)?, mountpoint, &options())?;
    Ok(())
}
