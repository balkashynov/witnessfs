use anyhow::Result;
use std::path::Path;
use witnessfs::{
    model::*,
    store::{Store, View},
};

fn write(v: &mut View<'_>, name: &str, body: &[u8]) -> Result<u64> {
    let n = match v.lookup(ROOT, name.as_bytes(), name)? {
        Some(n) => n,
        None => v.create(ROOT, name.as_bytes(), Kind::File, 0o644, 0, 0, vec![], name)?,
    };
    v.truncate(n.id, 0, name)?;
    v.write(n.id, 0, body, name)?;
    Ok(n.id)
}
fn read(v: &mut View<'_>, name: &str) -> Result<Vec<u8>> {
    let n = v.resolve(Path::new(name))?.unwrap();
    v.read(n.id, 0, MAX_IO_SIZE, name)
}
fn seed(store: &mut Store) -> Result<()> {
    store.branch("seed")?;
    store.transact("seed", |v| {
        write(v, "a", b"1")?;
        write(v, "b", b"1")?;
        Ok(())
    })?;
    accept(store, "seed");
    Ok(())
}
fn accept(store: &mut Store, id: &str) -> Receipt {
    store.seal(id).unwrap();
    store.accept(id).unwrap()
}

#[test]
fn independent_writers_both_commit() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut s = Store::create(&tmp.path().join("store"))?;
    seed(&mut s)?;
    s.branch("one")?;
    s.branch("two")?;
    s.transact("one", |v| write(v, "a", b"2"))?;
    s.transact("two", |v| write(v, "b", b"3"))?;
    assert_eq!(accept(&mut s, "one").outcome, "accepted");
    assert_eq!(accept(&mut s, "two").outcome, "accepted");
    Ok(())
}

#[test]
fn detects_write_skew_despite_disjoint_outputs() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut s = Store::create(&tmp.path().join("store"))?;
    seed(&mut s)?;
    s.branch("one")?;
    s.branch("two")?;
    s.transact("one", |v| {
        assert_eq!(read(v, "b")?, b"1");
        write(v, "a", b"0")
    })?;
    s.transact("two", |v| {
        assert_eq!(read(v, "a")?, b"1");
        write(v, "b", b"0")
    })?;
    assert_eq!(accept(&mut s, "one").outcome, "accepted");
    let receipt = accept(&mut s, "two");
    assert_eq!(receipt.outcome, "rejected");
    assert!(
        receipt
            .conflicts
            .iter()
            .any(|c| c.observation.kind == ObservationKind::Content)
    );
    s.branch("verify")?;
    s.transact("verify", |v| {
        assert_eq!(read(v, "a")?, b"0");
        assert_eq!(read(v, "b")?, b"1");
        Ok(())
    })?;
    Ok(())
}

#[test]
fn absence_and_directory_membership_are_dependencies() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut s = Store::create(&tmp.path().join("store"))?;
    for id in ["absence", "directory", "creator"] {
        s.branch(id)?;
    }
    s.transact("absence", |v| {
        assert!(v.lookup(ROOT, b"flag", "flag")?.is_none());
        write(v, "assumed", b"no flag")
    })?;
    s.transact("directory", |v| {
        assert!(v.children(ROOT, "/")?.is_empty());
        write(v, "inventory", b"empty")
    })?;
    s.transact("creator", |v| write(v, "flag", b"enabled"))?;
    assert_eq!(accept(&mut s, "creator").outcome, "accepted");
    assert_eq!(accept(&mut s, "absence").outcome, "rejected");
    assert!(
        accept(&mut s, "directory")
            .conflicts
            .iter()
            .any(|c| c.observation.kind == ObservationKind::Directory)
    );
    Ok(())
}

#[test]
fn failed_operation_keeps_observations_but_rolls_back_effects() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut s = Store::create(&tmp.path().join("store"))?;
    seed(&mut s)?;
    s.branch("reader")?;
    s.branch("writer")?;
    let err: Result<()> = s.transact("reader", |v| {
        read(v, "a")?;
        write(v, "side_effect", b"must rollback")?;
        anyhow::bail!("failure")
    });
    assert!(err.is_err());
    s.transact("reader", |v| {
        assert!(v.lookup(ROOT, b"side_effect", "side_effect")?.is_none());
        Ok(())
    })?;
    s.transact("writer", |v| write(v, "a", b"changed"))?;
    accept(&mut s, "writer");
    assert_eq!(accept(&mut s, "reader").outcome, "rejected");
    Ok(())
}

#[test]
fn sparse_writes_truncate_and_open_unlink_preserve_bytes() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut s = Store::create(&tmp.path().join("store"))?;
    s.branch("io")?;
    s.transact("io", |v| {
        let id = write(v, "file", b"abc")?;
        v.write(id, CHUNK_SIZE + 2, b"xyz", "file")?;
        assert_eq!(v.read(id, CHUNK_SIZE, 5, "file")?, b"\0\0xyz");
        v.truncate(id, 2, "file")?;
        v.truncate(id, CHUNK_SIZE + 5, "file")?;
        assert_eq!(v.read(id, 0, 3, "file")?, b"ab\0");
        assert_eq!(v.read(id, 7, 3, "file")?, b"\0\0\0");
        assert_eq!(v.read(id, CHUNK_SIZE, 5, "file")?, b"\0\0\0\0\0");
        v.unlink(ROOT, b"file", false, "file")?;
        assert!(v.lookup(ROOT, b"file", "file")?.is_none());
        assert_eq!(v.read(id, 0, 2, "(unlinked)")?, b"ab");
        Ok(())
    })?;
    Ok(())
}

#[test]
fn rename_and_hardlinks_retain_inode_identity() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let mut s = Store::create(&tmp.path().join("store"))?;
    s.branch("io")?;
    s.transact("io", |v| {
        let id = write(v, "file", b"abc")?;
        v.link(id, ROOT, b"alias", "alias")?;
        v.rename(ROOT, b"file", ROOT, b"moved", false, "file", "moved")?;
        assert_eq!(v.lookup(ROOT, b"moved", "moved")?.unwrap().id, id);
        v.write(id, 0, b"def", "moved")?;
        assert_eq!(read(v, "alias")?, b"def");
        v.unlink(ROOT, b"moved", false, "moved")?;
        assert_eq!(v.stat(id, "alias")?.nlink, 1);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn sealed_branch_and_mount_lock_prevent_acceptance_races() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("store");
    let mut s = Store::create(&path)?;
    s.branch("b")?;
    let lock = s.lock_branch("b")?;
    assert!(s.seal("b").is_err());
    assert!(s.accept("b").is_err());
    drop(lock);
    s.seal("b")?;
    assert!(s.transact("b", |v| write(v, "late", b"no")).is_err());
    let first = s.accept("b")?;
    let second = s.accept("b")?;
    assert_eq!(first.accepted_revision, second.accepted_revision);
    assert_eq!(s.head()?, 1);
    drop(s);
    let reopened = Store::open(&path)?;
    assert_eq!(reopened.integrity()?, "ok");
    assert_eq!(reopened.head()?, 1);
    Ok(())
}

#[test]
fn concurrent_acceptance_chooses_exactly_one_conflicting_writer() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("store");
    let mut s = Store::create(&path)?;
    seed(&mut s)?;
    for id in ["a", "b"] {
        s.branch(id)?;
        s.transact(id, |v| write(v, "a", id.as_bytes()))?;
        s.seal(id)?;
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut s = Store::open(&path).unwrap();
                barrier.wait();
                s.accept(id).unwrap().outcome
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        outcomes.iter().filter(|o| o.as_str() == "accepted").count(),
        1
    );
    Ok(())
}
