//! Descriptor-relative, no-follow operations inside administrator-owned roots.
use std::io::Read;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use rustix::fs::AtFlags;
use rustix::fs::Mode;
use rustix::fs::OFlags;

use super::*;

const LIMIT: u64 = 16 * 1024 * 1024;
static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn trusted_root(path: &Utf8Path, create: bool) -> io::Result<Option<OwnedFd>> {
    if create {
        fs::create_dir_all(path)?;
    }
    // Explicitly configured roots may use normal platform aliases (/var/run).
    // Every descendant is then accessed relative to this pinned directory FD.
    let path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(rustix::fs::open(path, DIRECTORY, Mode::empty())?))
}
fn directory(parent: &impl AsFd, name: &str, create: bool) -> io::Result<OwnedFd> {
    if create {
        match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o755)) {
            Ok(()) => {
                rustix::fs::fsync(parent)?;
            }
            Err(rustix::io::Errno::EXIST) => (),
            Err(error) => return Err(error.into()),
        }
    }
    let fd = rustix::fs::openat(parent, name, DIRECTORY, Mode::empty())?;
    if create {
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o755))?;
    }
    Ok(fd)
}
fn read(parent: &impl AsFd, name: &str) -> io::Result<Option<Vec<u8>>> {
    let fd = match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let file = fs::File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(invalid("metadata artifact is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("metadata artifact exceeds 16 MiB"));
    }
    Ok(Some(bytes))
}

fn ensure_file(parent: &impl AsFd, name: &str, content: &[u8]) -> io::Result<()> {
    if content.len() as u64 > LIMIT {
        return Err(invalid("metadata artifact exceeds 16 MiB"));
    }
    if let Some(existing) = read(parent, name)? {
        return if existing == content {
            Ok(())
        } else {
            Err(invalid(format!(
                "immutable metadata artifact changed: {name}"
            )))
        };
    }
    let mut attempts = 0;
    let (stage, fd) = loop {
        attempts += 1;
        if attempts > 16 {
            return Err(io::Error::other("metadata staging filename collisions"));
        }
        let stage = format!(
            ".stage-{name}-{}-{}",
            std::process::id(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        );
        match rustix::fs::openat(
            parent,
            &stage,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => break (stage, fd),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = (|| {
        let mut file = fs::File::from(fd);
        file.write_all(content)?;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        // A hard link publishes the complete inode atomically without replacing
        // an existing bind-mounted file (unlike rename with replacement).
        match rustix::fs::linkat(parent, &stage, parent, name, AtFlags::empty()) {
            Ok(()) => (),
            Err(rustix::io::Errno::EXIST) if read(parent, name)?.as_deref() == Some(content) => (),
            Err(error) => return Err(error.into()),
        }
        rustix::fs::fsync(parent)?;
        Ok(())
    })();
    let cleanup = rustix::fs::unlinkat(parent, &stage, AtFlags::empty()).map_err(io::Error::from);
    result.and(cleanup)
}
fn remove(parent: &impl AsFd, name: &str, directory: bool) -> io::Result<()> {
    let flags = if directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    match rustix::fs::unlinkat(parent, name, flags) {
        Ok(()) => {
            rustix::fs::fsync(parent)?;
            Ok(())
        }
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn remove_stages(parent: &impl AsFd, prefix: &str) -> io::Result<()> {
    for entry in rustix::fs::Dir::read_from(parent)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| invalid("invalid artifact filename"))?;
        if name.starts_with(prefix) {
            remove(parent, name, false)?;
        }
    }
    Ok(())
}
fn filename(driver: &str, uid: &str, request: &str) -> String {
    format!("{driver}_metadata_{uid}_{request}.json")
}

pub(super) fn persist(runtime: &MetadataRuntime, plan: &Plan) -> io::Result<()> {
    let local =
        trusted_root(&runtime.local, true)?.ok_or_else(|| invalid("missing plugin-data root"))?;
    let root = directory(&local, "dra-device-metadata", true)?;
    let claim = directory(&root, &plan.claim.uid, true)?;
    let cdi = trusted_root(&runtime.cdi, true)?.ok_or_else(|| invalid("missing CDI root"))?;
    // This immutable journal is sufficient for retry/restart replay. A mutable
    // ready flag adds no correctness: every publication is create-only.
    let journal = json::to_vec(&plan.journal(&runtime.driver)).map_err(io::Error::other)?;
    ensure_file(&claim, "journal.json", &journal)?;
    for (request, document) in &plan.documents {
        let dir = directory(&claim, request, true)?;
        ensure_file(&dir, "metadata.json", document.as_bytes())?;
        remove_stages(&dir, ".stage-metadata.json-")?;
    }
    // All data sources exist before the first CDI reference becomes visible.
    for (request, spec) in &plan.specs {
        let name = filename(&runtime.driver, &plan.claim.uid, request);
        ensure_file(&cdi, &name, spec.as_bytes())?;
        remove_stages(&cdi, &format!(".stage-{name}-"))?;
    }
    remove_stages(&claim, ".stage-journal.json-")?;
    Ok(())
}

pub(super) fn cleanup(runtime: &MetadataRuntime, identity: &ClaimRef) -> io::Result<()> {
    if !label(&identity.uid, 64) {
        return Err(invalid("invalid metadata claim UID"));
    }
    let Some(local) = trusted_root(&runtime.local, false)? else {
        return Ok(());
    };
    let root = match directory(&local, "dra-device-metadata", false) {
        Ok(fd) => fd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let claim = match directory(&root, &identity.uid, false) {
        Ok(fd) => fd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let Some(bytes) = read(&claim, "journal.json")? else {
        // A crash after removing the final journal can leave only an empty
        // UID directory. rmdir never traverses or deletes unknown contents.
        remove_stages(&claim, ".stage-journal.json-")?;
        return remove(&root, &identity.uid, true);
    };
    let journal = json::from_slice::<json::Value>(&bytes).map_err(io::Error::other)?;
    if journal["journalVersion"] != 1 {
        return Err(invalid(
            "unsupported metadata journal version; refusing cleanup",
        ));
    }
    if journal["driver"] != runtime.driver
        || journal["claim"]
            != json::json!({
                "name": identity.name,
                "namespace": identity.namespace,
                "uid": identity.uid
            })
    {
        return Err(invalid("metadata journal claim identity mismatch"));
    }
    let documents = journal["documents"]
        .as_object()
        .ok_or_else(|| invalid("invalid metadata journal documents"))?;
    let specs = journal["specs"]
        .as_object()
        .ok_or_else(|| invalid("invalid metadata journal specs"))?;
    if documents.len() != specs.len()
        || documents
            .keys()
            .any(|k| !label(k, 63) || !specs.contains_key(k))
    {
        return Err(invalid("invalid metadata journal request names"));
    }
    let cdi = trusted_root(&runtime.cdi, false)?;
    // Remove references first; retain the journal if any cleanup fails.
    if let Some(cdi) = &cdi {
        for request in specs.keys() {
            let name = filename(&runtime.driver, &identity.uid, request);
            if let Some(existing) = read(cdi, &name)? {
                let expected = specs[request]
                    .as_str()
                    .ok_or_else(|| invalid("invalid metadata journal spec"))?;
                if existing != expected.as_bytes() {
                    return Err(invalid("foreign or changed CDI spec; refusing cleanup"));
                }
                remove(cdi, &name, false)?;
            }
            remove_stages(cdi, &format!(".stage-{name}-"))?;
        }
    }
    for request in documents.keys() {
        let dir = match directory(&claim, request, false) {
            Ok(fd) => Some(fd),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if let Some(dir) = dir {
            remove(&dir, "metadata.json", false)?;
            remove_stages(&dir, ".stage-metadata.json-")?;
            remove(&claim, request, true)?;
        }
    }
    remove_stages(&claim, ".stage-journal.json-")?;
    // A foreign file must prevent final cleanup before the journal is removed.
    for entry in rustix::fs::Dir::read_from(&claim)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if ![
            b".".as_slice(),
            b"..".as_slice(),
            b"journal.json".as_slice(),
        ]
        .contains(&name)
        {
            return Err(invalid(
                "unknown file in metadata claim directory; journal retained",
            ));
        }
    }
    remove(&claim, "journal.json", false)?;
    remove(&root, &identity.uid, true)
}
