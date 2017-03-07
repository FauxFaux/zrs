extern crate regex;
extern crate tempfile;

use std::env;
use std::fs;
use std::io;
use std::path;
use std::time;
use std::vec;
use std::process::Command;

// magic functionality-adding imports:
use std::error::Error;
use std::io::BufRead;
use std::io::Write;

// cargo-culting macros woooo
macro_rules! println_stderr(
    ($($arg:tt)*) => { {
        let r = writeln!(&mut ::std::io::stderr(), $($arg)*);
        r.expect("failed printing to stderr");
    } }
);

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
    return time::SystemTime::now().duration_since(time::UNIX_EPOCH).unwrap().as_secs();
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

fn search(data_file: &path::PathBuf, expr: &str, mode: Scorer) -> io::Result<vec::Vec<ScoredRow>> {
    let table = try!(parse(data_file));

    let mut scored = vec::Vec::with_capacity(table.len());

    let re = try!(regex::Regex::new(expr).map_err(|e| io::Error::new(io::ErrorKind::Other, e.description())));

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

        scored.push(ScoredRow { path: row.path, score });
    }

    scored.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap());

    return Ok(scored);
}

fn usage(whoami: &str) {
    println_stderr!("usage: {} --add[-blocking] path", whoami);
}

#[derive(Debug)]
struct ParseError;

fn to_row(line: &str) -> Result<Row, ParseError> {
    let mut parts = line.split('|');

    let path: String = try!(parts.next().ok_or(ParseError)).to_string();

    let rank_part: &str = try!(parts.next().ok_or(ParseError));
    let rank: f32 = try!(rank_part.parse().map_err(|_| ParseError));

    let time_part: &str = try!(parts.next().ok_or(ParseError));
    let time: u64 = try!(time_part.parse().map_err(|_| ParseError));

    return Ok(Row { path, rank, time });
}

fn parse(data_file: &path::PathBuf) -> io::Result<vec::Vec<Row>> {
    let mut table: vec::Vec<Row> = vec::Vec::with_capacity(400);
    let fd = try!(fs::File::open(data_file));
    let reader = io::BufReader::new(&fd);

    for line in reader.lines() {
        let the_line = line.unwrap();
        let parsed = to_row(&the_line);
        if parsed.is_err() {
            continue;
        }
        let row = parsed.unwrap();

        // if something has stopped being a directory, drop it
        if !path::Path::new(&row.path).is_dir() {
            continue
        }

        table.push(row);
    }

    return Ok(table);
}

fn total_rank(table: &vec::Vec<Row>) -> f32 {
    let mut count: f32 = 0.0;
    for line in table {
        count += line.rank;
    }

    return count;
}

fn do_add(data_file: &path::PathBuf, what: &str) -> io::Result<()> {

    let mut table = try!(parse(data_file));

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
        table.push(Row { path: String::from(what), rank: 1.0, time: unix_time() });
    }

    // aging
    if total_rank(&table) > 9000.0 {
        for line in &mut table {
            line.rank *= 0.99;
        }
    }


    let tmp = tempfile::NamedTempFile::new_in(data_file.parent().unwrap())
        .expect("couldn't make a temporary file near data file");

    {
       let of = fs::File::create(tmp.path()).unwrap();

        let mut writer = io::BufWriter::new(&of);
        for line in table {
            if line.rank < 0.98 {
                continue
            }

            try!(write!(writer, "{}|{}|{}\n", line.path, line.rank, line.time));
        }
    }

    try!(tmp.persist(data_file));

    return Ok(());
}

enum Scorer {
    Rank,
    Recent,
    Frecent,
}

fn coded_main() -> u8 {
    let mut args = env::args();
    let arg_count = args.len();

    let data_file = match env::var("_Z_DATA") {
        Ok(x) => path::PathBuf::from(&x),
        Err(_) => {
            let home = env::home_dir().expect("home directory must be locatable");
            home.join(".z")
        },
    };

    let whoami = args.next().unwrap();
    let command = args.next().unwrap_or(String::new());
    if "--add" == command {
        if 3 != arg_count{
            usage(&whoami);
            return 2;
        }
        let arg = args.next().unwrap();
        Command::new(whoami)
            .args(&["--add-blocking", &arg])
            .spawn()
            .expect("helper failed to start");
        return 0;
    }

    if "--add-blocking" == command {
        if 3 != arg_count {
            usage(&whoami);
            return 2;
        }

        let err = do_add(&data_file, &args.next().unwrap());
        err.unwrap();
        return 0;
    }

    let mut mode = Scorer::Frecent;
    let mut subdirs = false;
    let mut list = false;
    let mut expr: String = String::new();

    let mut option = command;
    loop {
        if option.starts_with("-") {

            if option.len() < 2 {
                println_stderr!("invalid option: [no option]");
                return 3;
            }

            for c in option.chars().skip(1) {
                if 'c' == c {
                    subdirs = true;
                } else if 'l' == c {
                    list = true;
                } else if 'h' == c {
                    usage(&whoami);
                    return 2;
                } else if 'r' == c {
                    mode = Scorer::Rank;
                } else if 't' == c {
                    mode = Scorer::Recent;
                } else {
                    println_stderr!("unrecognised option: {}", option);
                    usage(&whoami);
                    return 3;
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

        let next = args.next();
        if next.is_none() {
            break;
        }
        option = next.unwrap();
    }

    if expr.is_empty() {
        list = true;
    }

    if subdirs {
        expr.insert_str(0, format!("^{}/.*", env::current_dir().unwrap().to_str().unwrap()).as_str());
    }

    println!("expr: {}", expr);

    let result = search(&data_file, expr.as_str(), mode).unwrap();
    if result.is_empty() {
        return 7;
    }

    if list {
        for row in result {
            println!("{:>10} {}", row.score, row.path);
        }
    } else {
        println!("{}", result[result.len() - 1].path);
    }

    return 0;
}

fn main() {
    std::process::exit(coded_main() as i32);
}
