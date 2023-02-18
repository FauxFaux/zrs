mod store;

use std::cmp;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::time;

use anyhow::anyhow;
use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use clap::{Arg, ArgAction};
use clap::ArgGroup;
use nix::unistd;

use crate::store::Row;

const HELPER_SCRIPT: &[u8] = include_bytes!("../z.sh");

#[derive(Debug)]
struct ScoredRow {
    path: PathBuf,
    score: f32,
}

#[derive(Copy, Clone)]
enum Scorer {
    Rank,
    Recent(u64),
    Frecent(u64),
}

impl Scorer {
    fn scored(self, row: Row) -> Result<ScoredRow> {
        let score = match self {
            Scorer::Rank => row.rank,
            Scorer::Recent(now) => -(time_delta(now, row.time) as f32),
            Scorer::Frecent(now) => frecent(row.rank, time_delta(now, row.time)),
        };

        ensure!(
            score.is_finite(),
            "computed non-finite score from {:?}",
            row
        );

        Ok(ScoredRow {
            path: row.path,
            score,
        })
    }
}

fn frecent(rank: f32, dx: u64) -> f32 {
    const HOUR: u64 = 3600;
    const DAY: u64 = HOUR * 24;
    const WEEK: u64 = DAY * 7;

    // relate frequency and time
    if dx < HOUR {
        rank * 4.0
    } else if dx < DAY {
        rank * 2.0
    } else if dx < WEEK {
        rank / 2.0
    } else {
        rank / 4.0
    }
}

fn search<P: AsRef<Path>>(data_file: P, expr: &str, mode: Scorer) -> Result<Vec<ScoredRow>> {
    let table =
        store::parse(store::open_data_file(data_file)?).with_context(|| anyhow!("parsing"))?;

    let mut matches: Vec<_> = {
        let sensitive = regex::RegexBuilder::new(expr)
            .case_insensitive(false)
            .build()
            .with_context(|| anyhow!("parsing regex: {:?}", expr))?;

        table
            .iter()
            .filter(|row| sensitive.is_match(&row.path.to_string_lossy()))
            .cloned()
            .collect()
    };

    if matches.is_empty() {
        let insensitive = regex::RegexBuilder::new(expr)
            .case_insensitive(true)
            .build()?;

        matches = table
            .into_iter()
            .filter(|row| insensitive.is_match(&row.path.to_string_lossy()))
            .collect();
    }

    let mut scored = matches
        .into_iter()
        .map(|row| mode.scored(row))
        .collect::<Result<Vec<_>>>()?;

    if let Some(prefix) = common_prefix(&scored) {
        if let Some(row) = scored.iter_mut().find(|row| prefix == row.path) {
            // if all of the matches have a common prefix,
            // and that common prefix is in the list,
            // then it is *much* more likely to be our guy.
            row.score *= 100.;
        }
    }

    scored.sort_by(compare_score);

    Ok(scored)
}

fn common_prefix(rows: &[ScoredRow]) -> Option<PathBuf> {
    if rows.len() <= 1 {
        return None;
    }

    let mut rows = rows.iter();
    let mut shortest = rows.next().expect("len > 1").path.to_path_buf();

    for part in rows {
        let part = part.path.to_path_buf();
        while !part.starts_with(&shortest) {
            if !shortest.pop() || shortest.parent().is_none() {
                return None;
            }
        }
    }

    Some(shortest)
}

fn total_rank(table: &[Row]) -> f32 {
    table.iter().map(|line| line.rank).sum()
}

fn do_add<Q: AsRef<Path>>(table: &mut Vec<Row>, what: Q) -> Result<()> {
    let what = what.as_ref();

    let found = match table.iter_mut().find(|row| row.path == what) {
        Some(row) => {
            row.rank += 1.0;
            row.time = unix_time();
            true
        }
        None => false,
    };

    if !found {
        table.push(Row {
            path: what.to_path_buf(),
            rank: 1.0,
            time: unix_time(),
        });
    }

    // aging
    if total_rank(table) > 9000.0 {
        for line in table {
            line.rank *= 0.99;
        }
    }

    Ok(())
}

fn run() -> Result<Return> {
    let data_file = match env::var_os("_Z_DATA") {
        Some(x) => PathBuf::from(&x),
        None => home_dir()?.join(".z"),
    };

    let matches = clap::command!()
        .group(ArgGroup::new("sort-mode").args(&["rank", "recent", "frecent"]))
        .arg(
            Arg::new("frecent")
                .short('f')
                .long("frecent")
                .action(ArgAction::SetTrue)
                .help("sort by a hybrid of the rank and age (default)"),
        )
        .arg(
            Arg::new("rank")
                .short('r')
                .long("rank")
                .action(ArgAction::SetTrue)
                .help("sort by the match's rank directly (ignore the time component)"),
        )
        .arg(
            Arg::new("recent")
                .short('t')
                .long("recent")
                .action(ArgAction::SetTrue)
                .help("sort by the match's age directly (ignore the rank component)"),
        )
        .arg(
            Arg::new("current-dir")
                .short('c')
                .long("current-dir")
                .action(ArgAction::SetTrue)
                .help("only return matches in the current dir"),
        )
        .arg(
            Arg::new("list")
                .short('l')
                .long("list")
                .action(ArgAction::SetTrue)
                .help("show all matching values"),
        )
        .arg(
            Arg::new("expressions")
                .num_args(0..)
                .help("terms to filter by"),
        )
        .arg(
            Arg::new("clean")
                .long("clean")
                .action(ArgAction::SetTrue)
                .help("remove entries which aren't dirs right now"),
        )
        .arg(
            Arg::new("add-to-profile")
                .long("add-to-profile")
                .hide_short_help(true)
                .action(ArgAction::SetTrue)
                .help("adds the helper script to the profile"),
        )
        .arg(
            Arg::new("add")
                .long("add")
                .hide_short_help(true)
                .value_name("PATH")
                .help("add a new entry to the database"),
        )
        .arg(
            Arg::new("add-blocking")
                .long("add-blocking")
                .hide_short_help(true)
                .value_name("PATH")
                .help("add a new entry, without forking"),
        )
        .arg(
            Arg::new("complete")
                .long("complete")
                .value_name("PREFIX")
                .hide_short_help(true)
                .help("the line we're trying to complete"),
        )
        .get_matches();

    {
        let blocking_add = matches.get_one::<&OsStr>("add-blocking");
        let normal_add = matches.get_one("add");
        if let Some(path) = normal_add.or(blocking_add) {
            // this must not be called while there are threaded operations running
            return add_entry(&data_file, blocking_add.is_none(), path);
        }
    }

    if let Some(line) = matches.get_one::<&str>("complete") {
        return complete(&data_file, line);
    }

    if matches.get_flag("clean") {
        return clean(&data_file);
    }

    if matches.get_flag("add-to-profile") {
        return add_to_profile();
    }

    let mode = if matches.get_flag("recent") {
        Scorer::Recent(unix_time())
    } else if matches.get_flag("rank") {
        Scorer::Rank
    } else {
        Scorer::Frecent(unix_time())
    };

    let mut list = matches.get_flag("list");
    let mut expr = String::new();

    if matches.get_flag("current-dir") {
        expr.push_str(&regex::escape(
            env::current_dir()
                .with_context(|| anyhow!("finding current dir"))?
                .to_str()
                .ok_or_else(|| anyhow!("current directory isn't valid utf-8"))?,
        ));
        expr.push('/');
    }

    if let Some(values) = matches.get_many::<&str>("expressions") {
        for val in values {
            if !expr.is_empty() {
                expr.push_str(".*");
            }
            expr.push_str(val);
        }
    } else {
        // even if there wasn't an explicit request to list, we had no expressions,
        // so we'll just print the whole thing
        list = true;
    }

    let table = search(&data_file, expr.as_str(), mode).with_context(|| anyhow!("main search"))?;

    if table.is_empty() {
        // It's empty!
        return Ok(Return::NoOutput);
    }

    if list {
        for row in table {
            println!("{:>10.3} {:?}", row.score, row.path);
        }
        Ok(Return::Success)
    } else {
        for row in table.into_iter().rev() {
            if !row.path.is_dir() {
                eprintln!("not a dir (run --clean to expunge): {:?}", row.path);
                continue;
            }
            println!("{}", row.path.to_string_lossy());

            return Ok(Return::DoCd);
        }

        Ok(Return::NoOutput)
    }
}

fn add_entry(data_file: &PathBuf, non_blocking_add: bool, path: &OsStr) -> Result<Return> {
    // this must not be called while there are threaded operations running
    if non_blocking_add && fork_is_parent().with_context(|| anyhow!("forking"))? {
        return Ok(Return::NoOutput);
    }

    store::update_file(data_file, |table| do_add(table, path))
        .with_context(|| anyhow!("adding to file"))?;

    Ok(Return::NoOutput)
}

fn complete(data_file: &PathBuf, mut line: &str) -> Result<Return> {
    let cmd = env::var("_Z_CMD").unwrap_or_else(|_err| "z".to_string());
    if line.starts_with(&cmd) {
        line = line[cmd.len()..].trim_start();
    }

    let escaped = regex::escape(line);

    for row in search(data_file, &escaped, Scorer::Frecent(unix_time()))
        .with_context(|| anyhow!("searching for completion data"))?
        .into_iter()
        .rev()
    {
        println!("{}", row.path.to_string_lossy());
    }

    Ok(Return::Success)
}

fn clean(data_file: &PathBuf) -> Result<Return> {
    let modified = store::update_file(data_file, |table| {
        let start = table.len();
        table.retain(|row| row.path.is_dir());
        Ok(start - table.len())
    })
    .with_context(|| anyhow!("cleaning data file"))?;

    println!(
        "Cleaned {} {}.",
        modified,
        if 1 == modified { "entry" } else { "entries" }
    );

    Ok(Return::Success)
}

fn add_to_profile() -> Result<Return> {
    let mut data =
        dirs::data_local_dir().ok_or_else(|| anyhow!("couldn't find your .local/share dir"))?;

    data.push("zrs");
    fs::create_dir_all(&data).with_context(|| anyhow!("creating {:?}", data))?;

    data.push("z.sh");
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&data)
        .with_context(|| anyhow!("opening {:?}", data))?
        .write_all(HELPER_SCRIPT)
        .with_context(|| anyhow!("writing helper script"))?;

    println!("written helper script to {:?}", data);

    let data = data
        .to_str()
        .ok_or_else(|| anyhow!("lazily refusing to handle non-utf8 paths"))?;
    ensure!(
        !data.contains('\''),
        "cowardly refusing to handle paths with single quotes"
    );

    let source_line = format!("\n\n. '{}'\n", data);

    let path = home_dir()?;

    for rc in &[".zshrc", ".bashrc"] {
        let mut path = path.to_path_buf();
        path.push(rc);
        match fs::read(&path) {
            Ok(current) => {
                if twoway::find_bytes(&current, data.as_bytes()).is_some() {
                    println!("appears to already be present, not appending: {:?}", path);
                    continue;
                }
            }
            Err(e) => {
                eprintln!("couldn't open {:?}: {:?}", path, e);
                continue;
            }
        }
        match fs::OpenOptions::new().append(true).open(&path) {
            Ok(mut zshrc) => {
                zshrc.write_all(source_line.as_bytes())?;
                drop(zshrc);
                println!("appended '. .../z.sh' to {:?}", path);
            }
            Err(e) => eprintln!("couldn't append to {:?}: {:?}", path, e),
        }
    }

    Ok(Return::Success)
}

fn compare_score(left: &ScoredRow, right: &ScoredRow) -> cmp::Ordering {
    left.score
        .partial_cmp(&right.score)
        .expect("no NaNs in scoring")
}

enum Return {
    DoCd,
    NoOutput,
    Success,
}

fn main() -> Result<()> {
    match run() {
        Ok(exit) => process::exit(match exit {
            Return::DoCd => 69,
            Return::NoOutput => 70,
            Return::Success => 0,
        }),
        Err(e) => Err(e),
    }
}

fn fork_is_parent() -> Result<bool> {
    // this is a cut-down version of unistd::daemon(),
    // except we return instead of exiting. Just being paranoid,
    // not actually expecting to be running long enough that this will matter.
    // Unsafe iff the parent's threads are doing other stuff. We don't have threads.
    match unsafe { unistd::fork()? } {
        unistd::ForkResult::Parent { .. } => Ok(true),
        unistd::ForkResult::Child => {
            env::set_current_dir("/")?;
            unistd::close(0)?;
            Ok(false)
        }
    }
}

fn unix_time() -> u64 {
    time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn time_delta(now: u64, then: u64) -> u64 {
    now.saturating_sub(then)
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("home directory must be locatable"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use super::ScoredRow;

    #[test]
    fn pathbuf_pop() {
        let mut p = PathBuf::from("/home/faux");
        assert!(p.pop());
        assert_eq!(PathBuf::from("/home"), p);
        assert!(p.pop());
        assert_eq!(PathBuf::from("/"), p);
        // a path for / has no parent, but `pop()` succeeded
        assert_eq!(None, p.parent());
        assert!(!p.pop());

        // further popping doesn't remove anything
        assert_eq!(PathBuf::from("/"), p);
    }

    #[test]
    fn common() {
        use super::common_prefix;
        assert_eq!(None, common_prefix(&[]));
        assert_eq!(None, common_prefix(&[s("/home")]));
        assert_eq!(None, common_prefix(&[s("/home"), s("/etc")]));
        assert_eq!(
            Some(PathBuf::from("/home")),
            common_prefix(&[s("/home/faux"), s("/home/john")])
        );

        assert_eq!(
            Some(PathBuf::from("/home")),
            common_prefix(&[
                s("/home/faux"),
                s("/home/alex/public_html"),
                s("/home/john"),
                s("/home/alex")
            ])
        );
    }

    fn s<P: AsRef<Path>>(path: P) -> ScoredRow {
        ScoredRow {
            path: path.as_ref().to_path_buf(),
            score: 0.,
        }
    }
}
