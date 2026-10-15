use crate::{
    model::*,
    store::{Store, View},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::Path,
};

pub fn import(store: &mut Store, source: &Path) -> Result<Receipt> {
    ensure!(source.is_dir(), "source must be a directory");
    ensure!(
        store.head()? == 0 && store.branches()?.is_empty(),
        "import requires an empty store"
    );
    let source = fs::canonicalize(source)?;
    let store_path = fs::canonicalize(&store.path)?;
    ensure!(
        !store_path.starts_with(&source),
        "store must be outside the imported tree"
    );
    store.branch("import")?;
    store.transact("import", |v| {
        import_dir(v, &source, ROOT, Path::new("/"), &mut HashMap::new())
    })?;
    store.seal("import")?;
    store.accept("import")
}

fn import_dir(
    v: &mut View<'_>,
    source: &Path,
    parent: u64,
    label: &Path,
    links: &mut HashMap<(u64, u64), u64>,
) -> Result<()> {
    let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let label = label.join(&name);
        let label_text = label.to_string_lossy();
        let kind = if metadata.is_dir() {
            Kind::Directory
        } else if metadata.is_file() {
            Kind::File
        } else if metadata.file_type().is_symlink() {
            Kind::Symlink
        } else {
            anyhow::bail!("unsupported special file: {}", path.display())
        };
        if kind == Kind::File
            && let Some(&ino) = links.get(&(metadata.dev(), metadata.ino()))
        {
            v.link(ino, parent, name.as_bytes(), &label_text)?;
            continue;
        }
        let target = if kind == Kind::Symlink {
            fs::read_link(&path)?.as_os_str().as_bytes().to_vec()
        } else {
            vec![]
        };
        let mut node = v.create(
            parent,
            name.as_bytes(),
            kind,
            metadata.mode(),
            unsafe { libc::getuid() },
            unsafe { libc::getgid() },
            target,
            &label_text,
        )?;
        match kind {
            Kind::Directory => import_dir(v, &path, node.id, &label, links)?,
            Kind::File => {
                ensure!(
                    metadata.len() <= MAX_FILE_SIZE,
                    "source file exceeds 1 GiB: {}",
                    path.display()
                );
                let mut file = File::open(&path)?;
                let mut buffer = vec![0; CHUNK_SIZE as usize];
                let mut offset = 0;
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    v.write(node.id, offset, &buffer[..n], &label_text)?;
                    offset += n as u64;
                }
                let after = file.metadata()?;
                ensure!(
                    metadata.len() == after.len()
                        && metadata.mtime() == after.mtime()
                        && metadata.mtime_nsec() == after.mtime_nsec(),
                    "source changed during import: {}",
                    path.display()
                );
                links.insert((metadata.dev(), metadata.ino()), node.id);
                node = v.node(node.id)?;
            }
            Kind::Symlink => {}
        }
        if metadata.mtime() >= 0 {
            node.mtime_ns = (metadata.mtime() as u64)
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.mtime_nsec() as u64);
        }
        v.set_node(node, &label_text)?;
    }
    Ok(())
}

pub fn export(store: &mut Store, branch: Option<&str>, destination: &Path) -> Result<()> {
    ensure!(!destination.exists(), "export destination must not exist");
    fs::create_dir_all(destination)?;
    store
        .inspect(branch, |v| {
            export_dir(v, ROOT, destination, Path::new("/"), &mut HashMap::new())
        })
        .context("export incomplete; destination contains a partial export")
}

fn export_dir(
    v: &mut View<'_>,
    ino: u64,
    destination: &Path,
    label: &Path,
    links: &mut HashMap<u64, std::path::PathBuf>,
) -> Result<()> {
    use std::os::unix::ffi::OsStringExt;
    for (name, node) in v.children(ino, &label.to_string_lossy())? {
        let name = std::ffi::OsString::from_vec(name);
        let path = destination.join(&name);
        let label = label.join(name);
        match node.kind {
            Kind::Directory => {
                fs::create_dir(&path)?;
                export_dir(v, node.id, &path, &label, links)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(node.mode))?;
            }
            Kind::Symlink => symlink(std::ffi::OsString::from_vec(node.target), &path)?,
            Kind::File => {
                if let Some(original) = links.get(&node.id) {
                    fs::hard_link(original, &path)?;
                    continue;
                }
                let mut f = File::create(&path)?;
                let mut offset = 0;
                while offset < node.size {
                    let data = v.read(node.id, offset, CHUNK_SIZE, &label.to_string_lossy())?;
                    ensure!(!data.is_empty(), "unexpected empty export read");
                    f.write_all(&data)?;
                    offset += data.len() as u64;
                }
                f.set_permissions(fs::Permissions::from_mode(node.mode))?;
                links.insert(node.id, path);
            }
        }
    }
    Ok(())
}
