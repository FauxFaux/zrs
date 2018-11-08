#[macro_use]
extern crate clap;
extern crate dirs;
#[macro_use]
extern crate failure;
extern crate nix;
extern crate regex;
extern crate tempfile;

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

use clap::Arg;
use clap::ArgGroup;
use failure::err_msg;
use failure::Error;
use failure::ResultExt;
use nix::unistd;

use store::Row;

const HELPER_SCRIPT: &'static [u8] = include_bytes!("../z.sh");

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
    fn scored(self, row: Row) -> Result<ScoredRow, Error> {
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

fn search<P: AsRef<Path>>(data_file: P, expr: &str, mode: Scorer) -> Result<Vec<ScoredRow>, Error> {
    let table = store::parse(fs::File::open(data_file).with_context(|_| err_msg("opening"))?)
        .with_context(|_| err_msg("parsing"))?;

    let mut matches: Vec<_> = {
        let sensitive = regex::RegexBuilder::new(expr)
            .case_insensitive(false)
            .build()
            .with_context(|_| format_err!("parsing regex: {:?}", expr))?;

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
        .collect::<Result<Vec<_>, Error>>()?;

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

    let mut rows = rows.into_iter();
    let mut shortest = rows.next().expect("len > 1").path.to_path_buf();

    for part in rows {
        let mut part = part.path.to_path_buf();
        while !part.starts_with(&shortest) {
            if !shortest.pop() || shortest.parent().is_none() {
                return None;
            }
        }
    }

    Some(shortest)
}

fn total_rank(table: &[Row]) -> f32 {
    table.into_iter().map(|line| line.rank).sum()
}

fn do_add<Q: AsRef<Path>>(table: &mut Vec<Row>, what: Q) -> Result<(), Error> {
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
    if total_rank(&table) > 9000.0 {
        for line in table {
            line.rank *= 0.99;
        }
    }

    Ok(())
}

fn run() -> Result<Return, Error> {
    let data_file = match env::var_os("_Z_DATA") {
        Some(x) => PathBuf::from(&x),
        None => home_dir()?.join(".z"),
    };

    let matches = clap::App::new(crate_name!())
        .version(crate_version!())
        .setting(clap::AppSettings::DeriveDisplayOrder)
        .setting(clap::AppSettings::DisableHelpSubcommand)
        .group(ArgGroup::with_name("sort-mode").args(&["rank", "recent", "frecent"]))
        .arg(
            Arg::with_name("frecent")
                .short("f")
                .long("frecent")
                .help("sort by a hybrid of the rank and age (default)"),
        )
        .arg(
            Arg::with_name("rank")
                .short("r")
                .long("rank")
                .help("sort by the match's rank directly (ignore the time component)"),
        )
        .arg(
            Arg::with_name("recent")
                .short("t")
                .long("recent")
                .help("sort by the match's age directly (ignore the rank component)"),
        )
        .arg(
            Arg::with_name("current-dir")
                .short("c")
                .long("current-dir")
                .help("only return matches in the current dir"),
        )
        .arg(
            Arg::with_name("list")
                .short("l")
                .long("list")
                .help("show all matching values"),
        )
        .arg(
            Arg::with_name("expressions")
                .multiple(true)
                .help("terms to filter by"),
        )
        .arg(
            Arg::with_name("clean")
                .long("clean")
                .help("remove entries which aren't dirs right now"),
        )
        .arg(
            Arg::with_name("add-to-profile")
                .long("add-to-profile")
                .hidden_short_help(true)
                .help("adds the helper script to the profile"),
        )
        .arg(
            Arg::with_name("add")
                .long("add")
                .hidden_short_help(true)
                .value_name("PATH")
                .help("add a new entry to the database"),
        )
        .arg(
            Arg::with_name("add-blocking")
                .long("add-blocking")
                .hidden_short_help(true)
                .value_name("PATH")
                .help("add a new entry, without forking"),
        )
        .arg(
            Arg::with_name("complete")
                .long("complete")
                .value_name("PREFIX")
                .hidden_short_help(true)
                .help("the line we're trying to complete"),
        )
        .get_matches();

    {
        let blocking_add = matches.value_of_os("add-blocking");
        let normal_add = matches.value_of_os("add");
        if let Some(path) = normal_add.or(blocking_add) {
            return add_entry(&data_file, blocking_add.is_none(), path);
        }
    }

    if let Some(mut line) = matches.value_of("complete") {
        return complete(&data_file, &mut line);
    }

    if matches.is_present("clean") {
        return clean(&data_file);
    }

    if matches.is_present("add-to-profile") {
        return add_to_profile();
    }

    let mode = if matches.is_present("recent") {
        Scorer::Recent(unix_time())
    } else if matches.is_present("rank") {
        Scorer::Rank
    } else {
        Scorer::Frecent(unix_time())
    };

    let mut list = matches.is_present("list");
    let mut expr = String::new();

    if matches.is_present("current-dir") {
        expr.push_str(&regex::escape(
            env::current_dir()
                .with_context(|_| err_msg("finding current dir"))?
                .to_str()
                .ok_or_else(|| err_msg("current directory isn't valid utf-8"))?,
        ));
        expr.push('/');
    }

    if let Some(values) = matches.values_of("expressions") {
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

    let table = search(&data_file, expr.as_str(), mode).with_context(|_| err_msg("main search"))?;

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

fn add_entry(data_file: &PathBuf, blocking_add: bool, path: &OsStr) -> Result<Return, Error> {
    if blocking_add {
        if fork_is_parent().with_context(|_| err_msg("forking"))? {
            return Ok(Return::NoOutput);
        }
    }

    store::update_file(data_file, |table| do_add(table, path))
        .with_context(|_| err_msg("adding to file"))?;

    Ok(Return::NoOutput)
}

fn complete(data_file: &PathBuf, mut line: &str) -> Result<Return, Error> {
    let cmd = env::var("_Z_CMD").unwrap_or_else(|_err| "z".to_string());
    if line.starts_with(&cmd) {
        line = &line[cmd.len()..].trim_left();
    }

    let escaped = regex::escape(line);

    for row in search(&data_file, &escaped, Scorer::Frecent(unix_time()))
        .with_context(|_| err_msg("searching for completion data"))?
        .into_iter()
        .rev()
    {
        println!("{}", row.path.to_string_lossy());
    }

    Ok(Return::Success)
}

fn clean(data_file: &PathBuf) -> Result<Return, Error> {
    let modified = store::update_file(data_file, |table| {
        let start = table.len();
        table.retain(|row| row.path.is_dir());
        Ok(start - table.len())
    })
    .with_context(|_| err_msg("cleaning data file"))?;

    println!(
        "Cleaned {} {}.",
        modified,
        if 1 == modified { "entry" } else { "entries" }
    );

    Ok(Return::Success)
}

fn add_to_profile() -> Result<Return, Error> {
    let mut data =
        dirs::data_local_dir().ok_or_else(|| err_msg("couldn't find your .local/share dir"))?;

    data.push("zrs");
    fs::create_dir_all(&data).with_context(|_| format_err!("creating {:?}", data))?;

    data.push("z.sh");
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&data)
        .with_context(|_| format_err!("opening {:?}", data))?
        .write_all(HELPER_SCRIPT)
        .with_context(|_| err_msg("writing helper script"))?;

    println!("written helper script to {:?}", data);

    let data = data
        .to_str()
        .ok_or_else(|| err_msg("lazily refusing to handle non-utf8 paths"))?;
    ensure!(
        !data.contains('\''),
        "cowardly refusing to handle paths with single quotes"
    );

    let data = format!("\n\n. '{}'\n", data);

    let mut path = home_dir()?;

    for rc in &[".zshrc", ".bashrc"] {
        path.push(rc);
        match fs::OpenOptions::new().append(true).open(&path) {
            Ok(mut zshrc) => {
                zshrc.write_all(data.as_bytes())?;
                drop(zshrc);
                println!("appended '. .../z.sh' to {:?}", path);
            }
            Err(e) => eprintln!("couldn't append to {:?}: {:?}", path, e),
        }
        path.pop();
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

fn main() -> Result<(), Error> {
    match run() {
        Ok(exit) => process::exit(match exit {
            Return::DoCd => 69,
            Return::NoOutput => 70,
            Return::Success => 0,
        }),
        Err(e) => Err(e),
    }
}

fn fork_is_parent() -> Result<bool, Error> {
    // this is a cut-down version of unistd::daemon(),
    // except we return instead of exiting. Just being paranoid,
    // not actually expecting to be running long enough that this will matter.
    match unistd::fork()? {
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
    now.checked_sub(then).unwrap_or(0)
}

fn home_dir() -> Result<PathBuf, Error> {
    dirs::home_dir().ok_or_else(|| err_msg("home directory must be locatable"))
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
