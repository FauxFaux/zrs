#[macro_use]
extern crate error_chain;
extern crate regex;
extern crate tempfile;

mod errors;

use std::cmp;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time;

use std::io::BufRead;
use std::io::Write;

use errors::*;

struct Row {
    path: PathBuf,
    rank: f32,
    time: u64,
}

struct ScoredRow {
    path: PathBuf,
    score: f32,
}

fn unix_time() -> u64 {
    return time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
}

fn frecent(rank: f32, dx: u64) -> f32 {
    // relate frequency and time
    if dx < 3600 {
        return rank * 4.0;
    }
    if dx < 86400 {
        return rank * 2.0;
    }
    if dx < 604800 {
        return rank / 2.0;
    }
    return rank / 4.0;
}

fn search(
    data_file: &PathBuf,
    expr: &str,
    mode: Scorer,
) -> Result<Box<Iterator<Item = Result<ScoredRow>>>> {
    let table = parse(data_file)?;

    let re = regex::Regex::new(expr)?;

    let now = unix_time();

    Ok(Box::new(
        table
            .filter(move |row| match row {
                &Ok(ref row) => re.is_match(&row.path.to_string_lossy()),
                &Err(_) => true,
            })
            .map(move |row| {
                row.map(|row| {
                    let score: f32 = match mode {
                        Scorer::Rank => row.rank,
                        Scorer::Recent => -((now - row.time) as f32),
                        Scorer::Frecent => frecent(row.rank, now - row.time),
                    };

                    ScoredRow {
                        path: row.path,
                        score,
                    }
                })
            }),
    ))
}

fn usage(whoami: &str) {
    eprintln!("usage: {} --add[-blocking] path", whoami);
}

fn to_row(line: &str) -> Result<Row> {
    let mut parts = line.split('|');
    return Ok(Row {
        path: PathBuf::from(parts.next().ok_or("row needs a path")?),
        rank: parts.next().ok_or("row needs a rank")?.parse()?,
        time: parts.next().ok_or("row needs a time")?.parse()?,
    });
}

struct IterTable {
    lines: io::Lines<io::BufReader<fs::File>>,
}

impl Iterator for IterTable {
    type Item = Result<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.lines.next() {
                Some(Ok(line)) => match to_row(&line) {
                    Ok(ref row) if !row.path.is_dir() => continue,
                    Ok(row) => return Some(Ok(row)),
                    Err(e) => eprintln!("couldn't parse {:?}: {:?}", line, e),
                },
                Some(Err(e)) => return Some(Err(Error::with_chain(e, "reading file"))),
                None => return None,
            }
        }
    }
}

fn parse(data_file: &PathBuf) -> Result<IterTable> {
    Ok(IterTable {
        lines: io::BufReader::new(fs::File::open(data_file)?).lines(),
    })
}

fn total_rank(table: &Vec<Row>) -> f32 {
    let mut count: f32 = 0.0;
    for line in table {
        count += line.rank;
    }

    return count;
}

fn do_add(data_file: &PathBuf, what: &PathBuf) -> Result<()> {
    let mut table: Vec<Row> = parse(data_file)?.collect::<Result<Vec<Row>>>()?;

    let mut found = false;
    for row in &mut table {
        if row.path != *what {
            continue;
        }
        row.rank += 1.0;
        row.time = unix_time();
        found = true;
        break;
    }

    // if we didn't find the thing to add, add it now
    if !found {
        table.push(Row {
            path: what.clone(),
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


    let tmp = tempfile::NamedTempFile::new_in(data_file
        .parent()
        .ok_or("data file cannot be at the root")?)
        .chain_err(|| {
        "couldn't make a temporary file near data file"
    })?;

    {
        let mut writer = io::BufWriter::new(&tmp);
        for line in table {
            if line.rank < 0.98 {
                continue;
            }

            writeln!(writer, "{:?}|{}|{}", line.path, line.rank, line.time)?;
        }
    }

    tmp.persist(data_file)?;

    return Ok(());
}

#[derive(Copy, Clone)]
enum Scorer {
    Rank,
    Recent,
    Frecent,
}

fn run() -> Result<i32> {
    let mut args = env::args();
    let arg_count = args.len();

    let data_file = match env::var_os("_Z_DATA") {
        Some(x) => PathBuf::from(&x),
        None => {
            let home = env::home_dir().chain_err(|| "home directory must be locatable")?;
            home.join(".z")
        }
    };

    let whoami = args.next().unwrap();
    let command = args.next().unwrap_or(String::new());
    if "--add" == command {
        if 3 != arg_count {
            usage(&whoami);
            return Ok(2);
        }
        let arg = args.next().chain_err(|| "--add takes an argument")?;
        Command::new(whoami)
            .args(&["--add-blocking", &arg])
            .spawn()
            .chain_err(|| "helper failed to start")?;
        return Ok(0);
    }

    if "--add-blocking" == command {
        if 3 != arg_count {
            usage(&whoami);
            return Ok(2);
        }

        do_add(
            &data_file,
            &args.next()
                .chain_err(|| "--add-blocking needs an argument")?
                .into(),
        )?;
        return Ok(0);
    }

    let mut mode = Scorer::Frecent;
    let mut subdirs = false;
    let mut list = false;
    let mut expr: String = String::new();

    let mut option = command;
    loop {
        if option.starts_with("-") {
            if option.len() < 2 {
                eprintln!("invalid option: [no option]");
                return Ok(3);
            }

            for c in option.chars().skip(1) {
                if 'c' == c {
                    subdirs = true;
                } else if 'l' == c {
                    list = true;
                } else if 'h' == c {
                    usage(&whoami);
                    return Ok(2);
                } else if 'r' == c {
                    mode = Scorer::Rank;
                } else if 't' == c {
                    mode = Scorer::Recent;
                } else {
                    eprintln!("unrecognised option: {}", option);
                    usage(&whoami);
                    return Ok(3);
                }
            }
        } else {
            if !expr.is_empty() {
                expr.push_str(".*");
            }
            if !option.is_empty() {
                expr.push_str(option.as_str());
            }
        }

        option = match args.next() {
            Some(option) => option,
            None => break,
        };
    }

    if expr.is_empty() {
        list = true;
    }

    if subdirs {
        expr.insert_str(
            0,
            format!(
                "^{}/.*",
                env::current_dir()?
                    .to_str()
                    .ok_or("current directory isn't valid utf-8")?
            ).as_str(),
        );
    }

    println!("expr: {}", expr);

    let mut result = search(&data_file, expr.as_str(), mode)?.collect::<Result<Vec<ScoredRow>>>()?;
    if result.is_empty() {
        return Ok(7);
    }

    if list {
        result.sort_by(compare_score);
        for row in result {
            println!("{:>10} {:?}", row.score, row.path);
        }
    } else {
        let best = result
            .into_iter()
            .max_by(compare_score)
            .expect("already checked if it was empty");
        println!("{:?}", best.path);
    }

    Ok(0)
}

fn compare_score(left: &ScoredRow, right: &ScoredRow) -> cmp::Ordering {
    match left.score.partial_cmp(&right.score) {
        Some(c) => c,
        None => left.score.is_nan().cmp(&right.score.is_nan()),
    }
}

quick_main!(run);
