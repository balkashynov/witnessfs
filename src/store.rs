use crate::model::*;
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub struct Store {
    conn: Connection,
    pub path: PathBuf,
    lease: Option<(String, File)>,
}

fn decode(s: Option<String>) -> Result<Option<Value>> {
    s.map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(Into::into)
}

fn at(conn: &Connection, revision: i64, key: &str) -> Result<Option<Value>> {
    let s: Option<Option<String>> = conn.query_row(
        "SELECT value FROM versions WHERE key=?1 AND revision<=?2 ORDER BY revision DESC LIMIT 1",
        params![key, revision], |r| r.get(0)).optional()?;
    decode(s.flatten())
}

fn overlay(conn: &Connection, branch: &str, key: &str) -> Result<Option<Option<Value>>> {
    let s: Option<Option<String>> = conn
        .query_row(
            "SELECT value FROM overlay WHERE branch=?1 AND key=?2",
            params![branch, key],
            |r| r.get(0),
        )
        .optional()?;
    s.map(decode).transpose()
}

fn prefix_at(conn: &Connection, revision: i64, prefix: &str) -> Result<BTreeMap<String, Value>> {
    let upper = format!("{prefix}~");
    let mut stmt = conn.prepare("SELECT v.key,v.value FROM versions v WHERE v.key>=?1 AND v.key<?2
        AND v.revision=(SELECT MAX(w.revision) FROM versions w WHERE w.key=v.key AND w.revision<=?3)")?;
    let rows = stmt.query_map(params![prefix, upper, revision], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut result = BTreeMap::new();
    for row in rows {
        let (key, value) = row?;
        if let Some(value) = decode(value)? {
            result.insert(key, value);
        }
    }
    Ok(result)
}

fn fingerprint(
    conn: &Connection,
    revision: i64,
    key: &str,
    kind: ObservationKind,
) -> Result<String> {
    if kind == ObservationKind::Directory {
        return Ok(digest_json(&prefix_at(conn, revision, key)?));
    }
    let value = at(conn, revision, key)?;
    Ok(match (&value, kind) {
        (Some(Value::Node(node)), ObservationKind::Metadata) => node.metadata_fingerprint(),
        (Some(Value::Node(node)), ObservationKind::Content) => {
            digest_json(&(node.kind, node.size, &node.chunks, &node.target))
        }
        _ => digest_json(&value),
    })
}

fn branch_info(conn: &Connection, branch: &str) -> Result<Branch> {
    conn.query_row(
        "SELECT id,base,state FROM branches WHERE id=?1",
        [branch],
        |r| {
            Ok(Branch {
                id: r.get(0)?,
                base_revision: r.get(1)?,
                state: r.get(2)?,
            })
        },
    )
    .optional()?
    .with_context(|| format!("unknown branch {branch}"))
}

pub struct View<'a> {
    tx: &'a Transaction<'a>,
    pub branch: &'a str,
    pub base: i64,
    pending: std::cell::RefCell<Vec<Observation>>,
    tracking: bool,
}

impl Store {
    pub fn create(path: &Path) -> Result<Self> {
        ensure!(!path.exists(), "store already exists: {}", path.display());
        fs::create_dir_all(path)?;
        fs::create_dir(path.join("locks"))?;
        let conn = Connection::open(path.join("witness.sqlite"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE meta (key TEXT PRIMARY KEY,value INTEGER NOT NULL);
            INSERT INTO meta VALUES('head',0),('next_inode',2),('format',1);
            CREATE TABLE versions (key TEXT NOT NULL,revision INTEGER NOT NULL,value TEXT,PRIMARY KEY(key,revision));
            CREATE TABLE branches (id TEXT PRIMARY KEY,base INTEGER NOT NULL,state TEXT NOT NULL);
            CREATE TABLE overlay (branch TEXT NOT NULL,key TEXT NOT NULL,value TEXT,PRIMARY KEY(branch,key));
            CREATE TABLE observations (branch TEXT NOT NULL,kind TEXT NOT NULL,key TEXT NOT NULL,expected TEXT NOT NULL,
                path TEXT NOT NULL,operation TEXT NOT NULL,PRIMARY KEY(branch,kind,key));
            CREATE TABLE blobs (hash TEXT PRIMARY KEY,data BLOB NOT NULL);
            CREATE TABLE receipts (branch TEXT PRIMARY KEY,json TEXT NOT NULL);
            CREATE INDEX versions_revision ON versions(revision);")?;
        let root = Node::new(
            ROOT,
            Kind::Directory,
            ROOT,
            0o755,
            unsafe { libc::getuid() },
            unsafe { libc::getgid() },
        );
        conn.execute(
            "INSERT INTO versions VALUES(?1,0,?2)",
            params![node_key(ROOT), serde_json::to_string(&Value::Node(root))?],
        )?;
        let mut store = Self {
            conn,
            path: path.to_path_buf(),
            lease: None,
        };
        store.configure()?;
        Ok(store)
    }
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.join("witness.sqlite").is_file(),
            "not a WitnessFS store: {}",
            path.display()
        );
        let conn = Connection::open_with_flags(
            path.join("witness.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        let mut store = Self {
            conn,
            path: path.to_path_buf(),
            lease: None,
        };
        store.configure()?;
        let format: i64 =
            store
                .conn
                .query_row("SELECT value FROM meta WHERE key='format'", [], |r| {
                    r.get(0)
                })?;
        ensure!(format == 1, "unsupported store format {format}");
        Ok(store)
    }
    fn configure(&mut self) -> Result<()> {
        self.conn.busy_timeout(std::time::Duration::from_secs(30))?;
        self.conn
            .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        Ok(())
    }
    pub fn head(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key='head'", [], |r| r.get(0))?)
    }
    pub fn branch(&mut self, id: &str) -> Result<Branch> {
        validate_branch(id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO branches SELECT ?1,value,'active' FROM meta WHERE key='head'",
            [id],
        )?;
        let b = branch_info(&tx, id)?;
        tx.commit()?;
        Ok(b)
    }
    pub fn branches(&self) -> Result<Vec<Branch>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id,base,state FROM branches ORDER BY id")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Branch {
                    id: r.get(0)?,
                    base_revision: r.get(1)?,
                    state: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn info(&self, id: &str) -> Result<Branch> {
        branch_info(&self.conn, id)
    }
    pub fn lock_branch(&self, id: &str) -> Result<File> {
        validate_branch(id)?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.join("locks").join(id))?;
        f.try_lock_exclusive().with_context(|| {
            format!("branch {id} is mounted or being managed; unmount before sealing or accepting")
        })?;
        Ok(f)
    }
    pub fn hold_branch(&mut self, id: &str) -> Result<()> {
        ensure!(self.lease.is_none(), "store already holds a branch lease");
        let lock = self.lock_branch(id)?;
        ensure!(self.info(id)?.state == "active", "branch is not active");
        self.lease = Some((id.into(), lock));
        Ok(())
    }
    pub fn seal(&mut self, id: &str) -> Result<()> {
        let _guard = self.lock_branch(id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let b = branch_info(&tx, id)?;
        ensure!(
            b.state == "active" || b.state == "sealed",
            "cannot seal {} branch",
            b.state
        );
        tx.execute("UPDATE branches SET state='sealed' WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn transact<T>(
        &mut self,
        id: &str,
        operation: impl FnOnce(&mut View<'_>) -> Result<T>,
    ) -> Result<T> {
        let _guard = if self.lease.as_ref().is_some_and(|(held, _)| held == id) {
            None
        } else {
            Some(self.lock_branch(id)?)
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let b = branch_info(&tx, id)?;
        ensure!(
            b.state == "active",
            "branch {id} is {}, not active",
            b.state
        );
        tx.execute_batch("SAVEPOINT operation")?;
        let view = &mut View {
            tx: &tx,
            branch: id,
            base: b.base_revision,
            pending: Default::default(),
            tracking: true,
        };
        let result = operation(view);
        let pending = view.pending.take();
        if result.is_err() {
            tx.execute_batch("ROLLBACK TO operation")?;
        }
        tx.execute_batch("RELEASE operation")?;
        for o in pending {
            tx.execute(
                "INSERT OR IGNORE INTO observations VALUES(?1,?2,?3,?4,?5,?6)",
                params![id, o.kind.as_str(), o.key, o.expected, o.path, o.operation],
            )?;
        }
        tx.commit()?;
        result
    }
    pub fn inspect<T>(
        &mut self,
        id: Option<&str>,
        operation: impl FnOnce(&mut View<'_>) -> Result<T>,
    ) -> Result<T> {
        let tx = self.conn.transaction()?;
        let base = if let Some(id) = id {
            branch_info(&tx, id)?.base_revision
        } else {
            tx.query_row("SELECT value FROM meta WHERE key='head'", [], |r| r.get(0))?
        };
        let result = operation(&mut View {
            tx: &tx,
            branch: id.unwrap_or(""),
            base,
            pending: Default::default(),
            tracking: false,
        })?;
        tx.rollback()?;
        Ok(result)
    }
    pub fn observations(&self, id: &str) -> Result<Vec<Observation>> {
        observations(&self.conn, id)
    }
    pub fn receipt(&self, id: &str) -> Result<Option<Receipt>> {
        let s: Option<String> = self
            .conn
            .query_row("SELECT json FROM receipts WHERE branch=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub fn accept(&mut self, id: &str) -> Result<Receipt> {
        let _guard = self.lock_branch(id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(s) = tx
            .query_row("SELECT json FROM receipts WHERE branch=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            return Ok(serde_json::from_str(&s)?);
        }
        let b = branch_info(&tx, id)?;
        ensure!(
            b.state == "sealed",
            "seal branch before acceptance (state {})",
            b.state
        );
        let head: i64 =
            tx.query_row("SELECT value FROM meta WHERE key='head'", [], |r| r.get(0))?;
        let observations = observations(&tx, id)?;
        let mut conflicts = Vec::new();
        for o in &observations {
            let observed = fingerprint(&tx, head, &o.key, o.kind)?;
            if observed != o.expected {
                conflicts.push(Conflict {
                    observation: o.clone(),
                    observed,
                });
            }
        }
        let keys_written = usize::try_from(tx.query_row(
            "SELECT count(*) FROM overlay WHERE branch=?1",
            [id],
            |r| r.get::<_, i64>(0),
        )?)?;
        let accepted = conflicts.is_empty();
        let revision = if accepted { Some(head + 1) } else { None };
        let receipt = Receipt {
            format: "witnessfs.receipt.v1".into(),
            branch: id.into(),
            base_revision: b.base_revision,
            checked_revision: head,
            accepted_revision: revision,
            outcome: if accepted { "accepted" } else { "rejected" }.into(),
            observations_checked: observations.len(),
            keys_written,
            conflicts,
        };
        if let Some(rev) = revision {
            tx.execute(
                "INSERT INTO versions SELECT key,?1,value FROM overlay WHERE branch=?2",
                params![rev, id],
            )?;
            fault_point("after_versions");
            tx.execute("UPDATE meta SET value=?1 WHERE key='head'", [rev])?;
            fault_point("after_head");
        }
        tx.execute(
            "UPDATE branches SET state=?1 WHERE id=?2",
            params![receipt.outcome, id],
        )?;
        tx.execute(
            "INSERT INTO receipts VALUES(?1,?2)",
            params![id, serde_json::to_string(&receipt)?],
        )?;
        fault_point("after_receipt");
        tx.commit()?;
        fault_point("after_commit");
        Ok(receipt)
    }
    pub fn integrity(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?)
    }
    pub fn committed(&self, key: &str) -> Result<Option<Value>> {
        at(&self.conn, self.head()?, key)
    }
    pub fn stats(&self) -> Result<serde_json::Value> {
        let mut counts = BTreeMap::new();
        for table in [
            "versions",
            "branches",
            "overlay",
            "observations",
            "blobs",
            "receipts",
        ] {
            counts.insert(
                table,
                self.conn
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                        r.get::<_, i64>(0)
                    })?,
            );
        }
        let bytes: i64 =
            self.conn
                .query_row("SELECT COALESCE(SUM(length(data)),0) FROM blobs", [], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::json!({"head":self.head()?,"rows":counts,"unique_content_bytes":bytes}))
    }
}

fn fault_point(point: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("WITNESS_FAULT_POINT").as_deref() == Ok(point) {
        use std::io::Write;
        eprintln!("WITNESS_FAILPOINT {point}");
        let _ = std::io::stderr().flush();
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
    }
    #[cfg(not(feature = "fault-injection"))]
    let _ = point;
}

fn observations(conn: &Connection, id: &str) -> Result<Vec<Observation>> {
    let mut stmt=conn.prepare("SELECT kind,key,expected,path,operation FROM observations WHERE branch=?1 ORDER BY kind,key")?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
        ))
    })?;
    rows.map(|row| {
        let (k, key, expected, path, operation) = row?;
        Ok(Observation {
            kind: ObservationKind::parse(&k)?,
            key,
            expected,
            path,
            operation,
        })
    })
    .collect()
}

impl View<'_> {
    fn raw(&self, key: &str) -> Result<Option<Value>> {
        match overlay(self.tx, self.branch, key)? {
            Some(v) => Ok(v),
            None => at(self.tx, self.base, key),
        }
    }
    fn observe(&self, key: &str, kind: ObservationKind, path: &str, operation: &str) -> Result<()> {
        if !self.tracking {
            return Ok(());
        }
        if kind != ObservationKind::Directory
            && kind != ObservationKind::Write
            && overlay(self.tx, self.branch, key)?.is_some()
        {
            return Ok(());
        }
        let expected = fingerprint(self.tx, self.base, key, kind)?;
        self.pending.borrow_mut().push(Observation {
            kind,
            key: key.into(),
            expected,
            path: path.into(),
            operation: operation.into(),
        });
        Ok(())
    }
    fn put(&self, key: &str, value: Option<Value>, path: &str) -> Result<()> {
        ensure!(self.tracking, "read-only view");
        self.observe(key, ObservationKind::Write, path, "write_preimage")?;
        let encoded = value.map(|v| serde_json::to_string(&v)).transpose()?;
        self.tx.execute("INSERT INTO overlay VALUES(?1,?2,?3) ON CONFLICT(branch,key) DO UPDATE SET value=excluded.value",
            params![self.branch,key,encoded])?;
        Ok(())
    }
    pub fn node(&self, ino: u64) -> Result<Node> {
        match self.raw(&node_key(ino))? {
            Some(Value::Node(n)) => Ok(n),
            _ => Err(FsError(libc::ENOENT, "inode not found").into()),
        }
    }
    pub fn stat(&self, ino: u64, path: &str) -> Result<Node> {
        self.observe(&node_key(ino), ObservationKind::Metadata, path, "stat")?;
        self.node(ino)
    }
    pub fn lookup(&self, parent: u64, name: &[u8], path: &str) -> Result<Option<Node>> {
        validate_name(name)?;
        let p = self.stat(parent, path)?;
        require(p.kind == Kind::Directory, libc::ENOTDIR, "not a directory")?;
        let key = dent_key(parent, name);
        self.observe(&key, ObservationKind::Value, path, "lookup")?;
        match self.raw(&key)? {
            Some(Value::Dentry(ino)) => Ok(Some(self.stat(ino, path)?)),
            None => Ok(None),
            _ => bail!("invalid dentry"),
        }
    }
    pub fn resolve(&self, path: &Path) -> Result<Option<Node>> {
        let mut node = self.stat(ROOT, "/")?;
        let mut label = String::new();
        for component in path.components() {
            match component {
                std::path::Component::RootDir | std::path::Component::CurDir => {}
                std::path::Component::Normal(name) => {
                    use std::os::unix::ffi::OsStrExt;
                    label.push('/');
                    label.push_str(&name.to_string_lossy());
                    let Some(next) = self.lookup(node.id, name.as_bytes(), &label)? else {
                        return Ok(None);
                    };
                    ensure!(
                        next.kind != Kind::Symlink,
                        "CLI resolution does not follow symlinks; use the mount"
                    );
                    node = next;
                }
                _ => bail!("path must not contain parent components"),
            }
        }
        Ok(Some(node))
    }
    pub fn children(&self, ino: u64, path: &str) -> Result<Vec<(Vec<u8>, Node)>> {
        require(
            self.stat(ino, path)?.kind == Kind::Directory,
            libc::ENOTDIR,
            "not a directory",
        )?;
        let prefix = dir_prefix(ino);
        self.observe(&prefix, ObservationKind::Directory, path, "readdir")?;
        let mut items = prefix_at(self.tx, self.base, &prefix)?;
        let mut stmt = self
            .tx
            .prepare("SELECT key,value FROM overlay WHERE branch=?1 AND key>=?2 AND key<?3")?;
        let rows = stmt.query_map(params![self.branch, prefix, format!("{prefix}~")], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (k, v) = row?;
            if let Some(v) = decode(v)? {
                items.insert(k, v);
            } else {
                items.remove(&k);
            }
        }
        items
            .into_iter()
            .map(|(key, value)| match value {
                Value::Dentry(id) => Ok((unhex(&key[prefix.len()..])?, self.node(id)?)),
                _ => bail!("invalid directory value"),
            })
            .collect()
    }
    #[allow(clippy::too_many_arguments)] // Mirrors the FUSE creation operation.
    pub fn create(
        &self,
        parent: u64,
        name: &[u8],
        kind: Kind,
        mode: u32,
        uid: u32,
        gid: u32,
        target: Vec<u8>,
        path: &str,
    ) -> Result<Node> {
        require(
            self.lookup(parent, name, path)?.is_none(),
            libc::EEXIST,
            "entry already exists",
        )?;
        require(
            self.node(parent)?.nlink > 0,
            libc::ENOENT,
            "parent was unlinked",
        )?;
        let ino: i64 = self.tx.query_row(
            "UPDATE meta SET value=value+1 WHERE key='next_inode' RETURNING value-1",
            [],
            |r| r.get(0),
        )?;
        let ino = u64::try_from(ino).context("inode counter overflow")?;
        let mut node = Node::new(ino, kind, parent, mode, uid, gid);
        if kind == Kind::Symlink {
            node.size = target.len() as u64;
            node.target = target;
        }
        self.put(&node_key(ino), Some(Value::Node(node.clone())), path)?;
        self.put(&dent_key(parent, name), Some(Value::Dentry(ino)), path)?;
        Ok(node)
    }
    pub fn open_content(&self, ino: u64, path: &str) -> Result<Node> {
        self.observe(&node_key(ino), ObservationKind::Content, path, "open")?;
        self.stat(ino, path)
    }
    pub fn read(&self, ino: u64, offset: u64, size: u64, path: &str) -> Result<Vec<u8>> {
        ensure!(size <= MAX_IO_SIZE, "read exceeds I/O limit");
        let node = self.node(ino)?;
        require(node.kind == Kind::File, libc::EISDIR, "not a regular file")?;
        self.observe(&node_key(ino), ObservationKind::Content, path, "read")?;
        let len = size.min(node.size.saturating_sub(offset));
        let mut result = vec![0; len as usize];
        let mut pos = 0u64;
        while pos < len {
            let absolute = offset + pos;
            let index = absolute / CHUNK_SIZE;
            let within = absolute % CHUNK_SIZE;
            let count = (CHUNK_SIZE - within).min(len - pos);
            if let Some(hash) = node.chunks.get(&index) {
                let data: Vec<u8> =
                    self.tx
                        .query_row("SELECT data FROM blobs WHERE hash=?1", [hash], |r| r.get(0))?;
                ensure!(
                    crate::model::hash(&data) == *hash,
                    "content checksum mismatch"
                );
                let available = (data.len() as u64).saturating_sub(within).min(count);
                if available > 0 {
                    result[pos as usize..(pos + available) as usize]
                        .copy_from_slice(&data[within as usize..(within + available) as usize]);
                }
            }
            pos += count;
        }
        Ok(result)
    }
    fn store_chunk(&self, data: &[u8]) -> Result<Option<String>> {
        // A chunk has a canonical representation with trailing zeroes omitted.
        // The inode's size and chunk index reconstruct holes and zero padding.
        let Some(last) = data.iter().rposition(|b| *b != 0) else {
            return Ok(None);
        };
        let data = &data[..=last];
        let hash = hash(data);
        self.tx.execute(
            "INSERT OR IGNORE INTO blobs VALUES(?1,?2)",
            params![hash, data],
        )?;
        Ok(Some(hash))
    }
    pub fn write(&self, ino: u64, offset: u64, data: &[u8], path: &str) -> Result<usize> {
        ensure!(data.len() as u64 <= MAX_IO_SIZE, "write exceeds I/O limit");
        let end = offset
            .checked_add(data.len() as u64)
            .context("write offset overflow")?;
        require(
            end <= MAX_FILE_SIZE,
            libc::EFBIG,
            "file size limit exceeded",
        )?;
        let mut node = self.node(ino)?;
        require(node.kind == Kind::File, libc::EISDIR, "not a regular file")?;
        if data.is_empty() {
            return Ok(0);
        }
        let mut pos = 0;
        while pos < data.len() {
            let abs = offset + pos as u64;
            let index = abs / CHUNK_SIZE;
            let within = (abs % CHUNK_SIZE) as usize;
            let count = (CHUNK_SIZE as usize - within).min(data.len() - pos);
            let mut chunk = vec![0; CHUNK_SIZE as usize];
            if let Some(h) = node.chunks.get(&index) {
                let old: Vec<u8> =
                    self.tx
                        .query_row("SELECT data FROM blobs WHERE hash=?1", [h], |r| r.get(0))?;
                ensure!(hash(&old) == *h, "content checksum mismatch");
                chunk[..old.len()].copy_from_slice(&old);
            }
            chunk[within..within + count].copy_from_slice(&data[pos..pos + count]);
            if let Some(h) = self.store_chunk(&chunk)? {
                node.chunks.insert(index, h);
            } else {
                node.chunks.remove(&index);
            }
            pos += count;
        }
        node.size = node.size.max(end);
        node.mtime_ns = now_ns();
        node.ctime_ns = node.mtime_ns;
        self.put(&node_key(ino), Some(Value::Node(node)), path)?;
        Ok(data.len())
    }
    pub fn truncate(&self, ino: u64, size: u64, path: &str) -> Result<Node> {
        require(
            size <= MAX_FILE_SIZE,
            libc::EFBIG,
            "file size limit exceeded",
        )?;
        let mut node = self.node(ino)?;
        require(node.kind == Kind::File, libc::EISDIR, "not a regular file")?;
        if size < node.size {
            node.chunks.retain(|i, _| *i * CHUNK_SIZE < size);
            let remainder = (size % CHUNK_SIZE) as usize;
            if remainder > 0 {
                let index = size / CHUNK_SIZE;
                if let Some(h) = node.chunks.get(&index).cloned() {
                    let mut data: Vec<u8> =
                        self.tx
                            .query_row("SELECT data FROM blobs WHERE hash=?1", [&h], |r| {
                                r.get(0)
                            })?;
                    ensure!(hash(&data) == h, "content checksum mismatch");
                    data.truncate(remainder);
                    if let Some(h) = self.store_chunk(&data)? {
                        node.chunks.insert(index, h);
                    } else {
                        node.chunks.remove(&index);
                    }
                }
            }
        }
        node.size = size;
        node.mtime_ns = now_ns();
        node.ctime_ns = node.mtime_ns;
        self.put(&node_key(ino), Some(Value::Node(node.clone())), path)?;
        Ok(node)
    }
    pub fn set_node(&self, node: Node, path: &str) -> Result<()> {
        self.put(&node_key(node.id), Some(Value::Node(node)), path)
    }
    pub fn readlink(&self, ino: u64, path: &str) -> Result<Vec<u8>> {
        self.observe(&node_key(ino), ObservationKind::Content, path, "readlink")?;
        let node = self.node(ino)?;
        require(node.kind == Kind::Symlink, libc::EINVAL, "not a symlink")?;
        Ok(node.target)
    }
    pub fn unlink(&self, parent: u64, name: &[u8], directory: bool, path: &str) -> Result<()> {
        let mut node = self
            .lookup(parent, name, path)?
            .ok_or(FsError(libc::ENOENT, "entry not found"))?;
        require(
            directory == (node.kind == Kind::Directory),
            if directory {
                libc::ENOTDIR
            } else {
                libc::EISDIR
            },
            "wrong unlink kind",
        )?;
        if directory {
            require(
                self.children(node.id, path)?.is_empty(),
                libc::ENOTEMPTY,
                "directory not empty",
            )?;
        }
        self.put(&dent_key(parent, name), None, path)?;
        node.nlink = node.nlink.checked_sub(1).context("invalid link count")?;
        node.ctime_ns = now_ns();
        self.set_node(node, path)
    }
    pub fn link(&self, ino: u64, parent: u64, name: &[u8], path: &str) -> Result<Node> {
        let mut node = self.stat(ino, path)?;
        require(
            node.kind != Kind::Directory,
            libc::EPERM,
            "hard-linked directories unsupported",
        )?;
        require(
            self.lookup(parent, name, path)?.is_none(),
            libc::EEXIST,
            "entry already exists",
        )?;
        require(
            self.node(parent)?.nlink > 0,
            libc::ENOENT,
            "parent was unlinked",
        )?;
        node.nlink = node.nlink.checked_add(1).context("link count overflow")?;
        node.ctime_ns = now_ns();
        self.put(&dent_key(parent, name), Some(Value::Dentry(ino)), path)?;
        self.set_node(node.clone(), path)?;
        Ok(node)
    }
    #[allow(clippy::too_many_arguments)] // Two namespace endpoints and their diagnostic labels.
    pub fn rename(
        &self,
        parent: u64,
        name: &[u8],
        new_parent: u64,
        new_name: &[u8],
        no_replace: bool,
        path: &str,
        new_path: &str,
    ) -> Result<()> {
        let mut source = self
            .lookup(parent, name, path)?
            .ok_or(FsError(libc::ENOENT, "source not found"))?;
        let target = self.lookup(new_parent, new_name, new_path)?;
        require(
            self.node(new_parent)?.nlink > 0,
            libc::ENOENT,
            "parent was unlinked",
        )?;
        if no_replace {
            require(target.is_none(), libc::EEXIST, "destination already exists")?;
        }
        if target.as_ref().is_some_and(|n| n.id == source.id) {
            return Ok(());
        }
        if source.kind == Kind::Directory {
            let mut ancestor = new_parent;
            loop {
                require(
                    ancestor != source.id,
                    libc::EINVAL,
                    "cannot move directory into itself",
                )?;
                if ancestor == ROOT {
                    break;
                }
                ancestor = self.stat(ancestor, new_path)?.parent;
            }
        }
        if let Some(target) = target {
            require(
                (target.kind == Kind::Directory) == (source.kind == Kind::Directory),
                if source.kind == Kind::Directory {
                    libc::ENOTDIR
                } else {
                    libc::EISDIR
                },
                "incompatible rename target",
            )?;
            self.unlink(
                new_parent,
                new_name,
                target.kind == Kind::Directory,
                new_path,
            )?;
        }
        self.put(&dent_key(parent, name), None, path)?;
        self.put(
            &dent_key(new_parent, new_name),
            Some(Value::Dentry(source.id)),
            new_path,
        )?;
        source.parent = new_parent;
        source.ctime_ns = now_ns();
        self.set_node(source, new_path)
    }
}
