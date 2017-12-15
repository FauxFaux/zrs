#[macro_use]
extern crate error_chain;
extern crate regex;
extern crate tempfile;

mod errors;

use std::env;
use std::fs;
use std::io;
use std::path;
use std::time;

use std::io::BufRead;
use std::io::Write;
use std::process::Command;

use errors::*;

struct Row {
    path: String,
    rank: f32,
    time: u64,
}

struct ScoredRow {
    path: String,
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

fn search(data_file: &path::PathBuf, expr: &str, mode: Scorer) -> Result<Vec<ScoredRow>> {
    let table = parse(data_file)?;

    let mut scored = Vec::with_capacity(table.len());

    let re = regex::Regex::new(expr)?;

    let now = unix_time();
    for row in table {
        if !re.is_match(row.path.as_str()) {
            continue;
        }

        let score: f32 = match mode {
            Scorer::Rank => row.rank,
            Scorer::Recent => -((now - row.time) as f32),
            Scorer::Frecent => frecent(row.rank, now - row.time),
        };

        scored.push(ScoredRow {
            path: row.path,
            score,
        });
    }

    scored.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap());

    return Ok(scored);
}

fn usage(whoami: &str) {
    eprintln!("usage: {} --add[-blocking] path", whoami);
}

fn to_row(line: &str) -> Result<Row> {
    let mut parts = line.split('|');
    return Ok(Row {
        path: parts.next().ok_or("row needs a path")?.to_string(),
        rank: parts.next().ok_or("row needs a rank")?.parse()?,
        time: parts.next().ok_or("row needs a time")?.parse()?,
    });
}

fn parse(data_file: &path::PathBuf) -> io::Result<Vec<Row>> {
    let mut table: Vec<Row> = Vec::with_capacity(400);
    let fd = fs::File::open(data_file)?;
    let reader = io::BufReader::new(&fd);

    for line in reader.lines() {
        let parsed = to_row(&line?);
        if parsed.is_err() {
            continue;
        }
        let row = parsed.unwrap();

        // if something has stopped being a directory, drop it
        if !path::Path::new(&row.path).is_dir() {
            continue;
        }

        table.push(row);
    }

    return Ok(table);
}

fn total_rank(table: &Vec<Row>) -> f32 {
    let mut count: f32 = 0.0;
    for line in table {
        count += line.rank;
    }

    return count;
}

fn do_add(data_file: &path::PathBuf, what: &str) -> Result<()> {
    let mut table = parse(data_file)?;

    let mut found = false;
    for row in &mut table {
        if row.path != what {
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
            path: String::from(what),
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
        let of = fs::File::create(tmp.path())?;

        let mut writer = io::BufWriter::new(&of);
        for line in table {
            if line.rank < 0.98 {
                continue;
            }

            writeln!(writer, "{}|{}|{}", line.path, line.rank, line.time)?;
        }
    }

    tmp.persist(data_file)?;

    return Ok(());
}

enum Scorer {
    Rank,
    Recent,
    Frecent,
}

fn run() -> Result<i32> {
    let mut args = env::args();
    let arg_count = args.len();

    let data_file = match env::var_os("_Z_DATA") {
        Some(x) => path::PathBuf::from(&x),
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
                .chain_err(|| "--add-blocking needs an argument")?,
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

    let result = search(&data_file, expr.as_str(), mode).unwrap();
    if result.is_empty() {
        return Ok(7);
    }

    if list {
        for row in result {
            println!("{:>10} {}", row.score, row.path);
        }
    } else {
        println!("{}", result[result.len() - 1].path);
    }

    Ok(0)
}

quick_main!(run);
