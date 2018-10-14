#[macro_use]
extern crate clap;
extern crate dirs;
#[macro_use]
extern crate failure;
extern crate nix;
extern crate regex;
extern crate tempfile;

use std::cmp;
use std::env;
use std::fs;
use std::io;
use std::io::BufRead;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::time;

use clap::Arg;
use clap::ArgGroup;
use clap::SubCommand;
use failure::Error;
use failure::ResultExt;
use nix::unistd;

#[derive(Debug)]
struct Row {
    path: PathBuf,
    rank: f32,
    time: u64,
}

#[derive(Debug)]
struct ScoredRow {
    path: PathBuf,
    score: f32,
}

#[derive(Copy, Clone)]
enum Scorer {
    Rank,
    Recent,
    Frecent,
}

impl Row {
    fn into_scored(self, mode: Scorer, now: u64) -> ScoredRow {
        ScoredRow {
            path: self.path,
            score: match mode {
                Scorer::Rank => self.rank,
                Scorer::Recent => -((now - self.time) as f32),
                Scorer::Frecent => frecent(self.rank, now - self.time),
            }
            .assert_finite(),
        }
    }
}

fn unix_time() -> u64 {
    time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
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
    let table = parse(data_file)?;

    let re = regex::Regex::new(expr)?;

    let now = unix_time();

    Ok(table
        .into_iter()
        .filter_map(|row| {
            if re.is_match(&row.path.to_string_lossy()) {
                Some(row.into_scored(mode, now))
            } else {
                None
            }
        })
        .collect())
}

fn to_row(line: &str) -> Result<Row, Error> {
    let mut parts = line.split('|');
    Ok(Row {
        path: PathBuf::from(
            parts
                .next()
                .ok_or_else(|| format_err!("row needs a path"))?,
        ),
        rank: parts
            .next()
            .ok_or_else(|| format_err!("row needs a rank"))?
            .parse::<f32>()?
            .assert_finite(),
        time: parts
            .next()
            .ok_or_else(|| format_err!("row needs a time"))?
            .parse()?,
    })
}

fn parse<P: AsRef<Path>>(data_file: P) -> Result<Vec<Row>, Error> {
    let mut ret = Vec::with_capacity(500);
    for line in io::BufReader::new(fs::File::open(data_file)?).lines() {
        let line = line?;
        match to_row(&line) {
            Ok(row) => ret.push(row),
            Err(e) => eprintln!("couldn't parse {:?}: {:?}", line, e),
        }
    }

    Ok(ret)
}

fn total_rank(table: &[Row]) -> f32 {
    table.into_iter().map(|line| line.rank).sum()
}

fn do_add<P: AsRef<Path>, Q: AsRef<Path>>(data_file: P, what: Q) -> Result<(), Error> {
    let mut table = parse(&data_file)?;
    let what = what.as_ref();

    // TODO: borrow checker fail.
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
        for line in &mut table {
            line.rank *= 0.99;
        }
    }

    let tmp = tempfile::NamedTempFile::new_in(
        data_file
            .as_ref()
            .parent()
            .ok_or_else(|| format_err!("data file cannot be at the root"))?,
    )
    .with_context(|_| format_err!("couldn't make a temporary file near data file"))?;

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
            writeln!(writer, "{}|{}|{}", path, line.rank, line.time)?;
        }
    }

    tmp.persist(data_file)?;

    Ok(())
}

fn run() -> Result<i32, Error> {
    let data_file = match env::var_os("_Z_DATA") {
        Some(x) => PathBuf::from(&x),
        None => {
            let home =
                dirs::home_dir().ok_or_else(|| format_err!("home directory must be locatable"))?;
            home.join(".z")
        }
    };

    let matches = clap::App::new(crate_name!())
        .version(crate_version!())
        .setting(clap::AppSettings::ArgsNegateSubcommands)
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
        .subcommand(
            SubCommand::with_name("add")
                .help("add a new entry to the database")
                .arg(
                    Arg::with_name("blocking")
                        .short("b")
                        .long("blocking")
                        .help("actually do the add"),
                )
                .arg(Arg::with_name("path").required(true)),
        )
        .get_matches();

    match matches.subcommand() {
        ("add", Some(matches)) => {
            if !matches.is_present("blocking") {
                // TODO: reexec on platforms without nix?

                // this is a cut-down version of unistd::daemon(),
                // except we return instead of exiting. Just being paranoid,
                // not actually expecting to be running long enough that this will matter.
                match unistd::fork()? {
                    unistd::ForkResult::Parent { .. } => return Ok(0),
                    unistd::ForkResult::Child => {
                        env::set_current_dir("/")?;
                        unistd::close(0)?;
                    }
                }
            }

            let path = matches.value_of_os("path").expect("required argument");
            do_add(&data_file, path)?;
            return Ok(0);
        }

        ("", None) => (),
        _ => unreachable!(),
    }

    let mode = if matches.is_present("recent") {
        Scorer::Recent
    } else if matches.is_present("rank") {
        Scorer::Rank
    } else {
        Scorer::Frecent
    };

    let mut list = matches.is_present("list");
    let mut expr = String::new();

    if matches.is_present("current-dir") {
        expr.push_str(&regex::escape(
            env::current_dir()?
                .to_str()
                .ok_or_else(|| format_err!("current directory isn't valid utf-8"))?,
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
        list = true;
    }

    let mut table = search(&data_file, expr.as_str(), mode)?;

    if table.is_empty() {
        // It's empty!
        return Ok(7);
    }

    if list {
        table.sort_by(compare_score);
        for row in table {
            println!("{:>10.3} {:?}", row.score, row.path);
        }
    } else {
        let best = table
            .into_iter()
            .max_by(compare_score)
            .expect("already checked if it was empty");
        println!("{}", best.path.to_string_lossy());
    }

    Ok(0)
}

fn compare_score(left: &ScoredRow, right: &ScoredRow) -> cmp::Ordering {
    left.score
        .partial_cmp(&right.score)
        .expect("no NaNs in scoring")
}

fn main() -> Result<(), Error> {
    match run() {
        Ok(exit) => process::exit(exit),
        Err(e) => Err(e),
    }
}

trait FloatAnger {
    fn assert_finite(&self) -> f32;
}

impl FloatAnger for f32 {
    fn assert_finite(&self) -> f32 {
        assert!(self.is_finite());
        *self
    }
}
