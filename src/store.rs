use std::fs;
use std::io;
use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::mem;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::path::PathBuf;

use failure::err_msg;
use failure::Error;
use failure::ResultExt;
use nix::fcntl;
use tempfile::NamedTempFile;

#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    pub rank: f32,
    pub time: u64,
}

fn to_row(line: &str) -> Result<Row, Error> {
    let mut parts = line.split('|');

    let path = PathBuf::from(parts.next().ok_or_else(|| err_msg("row needs a path"))?);

    let rank = parts
        .next()
        .ok_or_else(|| err_msg("row needs a rank"))?
        .parse::<f32>()?;

    let time = parts
        .next()
        .ok_or_else(|| err_msg("row needs a time"))?
        .parse()?;

    ensure!(
        rank.is_finite(),
        "file contained non-finite rank: {:?}",
        rank
    );

    Ok(Row { path, rank, time })
}

pub fn parse<R: Read>(data_file: R) -> Result<Vec<Row>, Error> {
    let mut ret = Vec::with_capacity(500);
    for line in io::BufReader::new(data_file).lines() {
        let line = line.with_context(|_| err_msg("IO error during read"))?;
        match to_row(&line) {
            Ok(row) => ret.push(row),
            Err(e) => eprintln!("couldn't parse {:?}: {:?}", line, e),
        }
    }

    Ok(ret)
}

pub fn update_file<P: AsRef<Path>, F, R>(data_file: P, apply: F) -> Result<R, Error>
where
    F: FnOnce(&mut Vec<Row>) -> Result<R, Error>,
{
    let lock = fs::File::open(&data_file).with_context(|_| err_msg("opening"))?;
    fcntl::flock(lock.as_raw_fd(), fcntl::FlockArg::LockExclusive)
        .with_context(|_| err_msg("locking"))?;

    // Mmm, if we pass this by value, it will be dropped immediately, which we don't want
    let mut table = parse(&lock).with_context(|_| err_msg("parsing"))?;

    let result = apply(&mut table).with_context(|_| err_msg("processing"))?;

    let tmp = NamedTempFile::new_in(
        data_file
            .as_ref()
            .parent()
            .ok_or_else(|| err_msg("data file cannot be at the root"))?,
    )
    .with_context(|_| err_msg("couldn't make a temporary file near data file"))?;

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
                .with_context(|_| err_msg("writing temporary value"))?;
        }
    }

    tmp.persist(data_file)
        .with_context(|_| err_msg("replacing"))?;

    // just being explicit about when we expect the lock to live to
    mem::drop(lock);

    Ok(result)
}
