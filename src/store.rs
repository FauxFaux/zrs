use std::fs;
use std::io;
use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::mem;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;

use anyhow::anyhow;
use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use nix::fcntl;
use tempfile::NamedTempFile;

#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    pub rank: f32,
    pub time: u64,
}

fn to_row(line: &str) -> Result<Row> {
    let mut parts = line.split('|');

    let path = PathBuf::from(parts.next().ok_or_else(|| anyhow!("row needs a path"))?);

    let rank = parts
        .next()
        .ok_or_else(|| anyhow!("row needs a rank"))?
        .parse::<f32>()?;

    let time = parts
        .next()
        .ok_or_else(|| anyhow!("row needs a time"))?
        .parse()?;

    ensure!(
        rank.is_finite(),
        "file contained non-finite rank: {:?}",
        rank
    );

    Ok(Row { path, rank, time })
}

pub fn parse<R: Read>(data_file: R) -> Result<Vec<Row>> {
    let mut ret = Vec::with_capacity(500);
    for line in io::BufReader::new(data_file).lines() {
        let line = line.with_context(|| anyhow!("IO error during read"))?;
        match to_row(&line) {
            Ok(row) => ret.push(row),
            Err(e) => eprintln!("couldn't parse {:?}: {:?}", line, e),
        }
    }

    Ok(ret)
}

pub fn update_file<P: AsRef<Path>, F, R>(data_file: P, apply: F) -> Result<R>
where
    F: FnOnce(&mut Vec<Row>) -> Result<R>,
{
    let lock = open_data_file(&data_file)?;
    let lock = fcntl::Flock::lock(lock, fcntl::FlockArg::LockExclusive)
        .map_err(|(_, e)| e)
        .with_context(|| anyhow!("locking"))?;

    // Mmm, if we pass this by value, it will be dropped immediately, which we don't want
    let mut table = parse(lock.deref()).with_context(|| anyhow!("parsing"))?;

    let result = apply(&mut table).with_context(|| anyhow!("processing"))?;

    let tmp = NamedTempFile::new_in(
        data_file
            .as_ref()
            .parent()
            .ok_or_else(|| anyhow!("data file cannot be at the root"))?,
    )
    .with_context(|| anyhow!("couldn't make a temporary file near data file"))?;

    {
        let mut writer = io::BufWriter::new(&tmp);
        for line in table {
            if line.rank < 0.98 {
                continue;
            }

            let path = match line.path.to_str() {
                Some(path) if path.contains('|') || path.contains('\n') => continue,
                Some(path) => path,
                None => continue,
            };
            writeln!(writer, "{}|{}|{}", path, line.rank, line.time)
                .with_context(|| anyhow!("writing temporary value"))?;
        }
    }

    // best effort attempt to maintain uid/gid
    // TODO: other attributes; mode is handled by umask.. maybe.
    if let Ok(stat) = nix::sys::stat::stat(data_file.as_ref()) {
        let _ = nix::unistd::chown(
            tmp.path(),
            Some(nix::unistd::Uid::from_raw(stat.st_uid)),
            Some(nix::unistd::Gid::from_raw(stat.st_gid)),
        );
    }

    tmp.persist(data_file)
        .with_context(|| anyhow!("replacing"))?;

    // just being explicit about when we expect the lock to live to
    mem::drop(lock);

    Ok(result)
}

pub fn open_data_file<P: AsRef<Path>>(data_file: P) -> Result<fs::File> {
    let data_file = data_file.as_ref();
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(data_file)
        .with_context(|| anyhow!("opening/creating data file at {:?}", data_file))
}
