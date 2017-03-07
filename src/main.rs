extern crate tempfile;

use std::env;
use std::fs;
use std::io;
use std::path;
use std::thread;
use std::time;
use std::vec;
use std::process::Command;

// magic functionality-adding imports:
use std::io::BufRead;
use std::io::Write;

struct Row {
    path: String,
    rank: f32,
    time: u64,
}

fn unix_time() -> u64 {
    return time::SystemTime::now().duration_since(time::UNIX_EPOCH).unwrap().as_secs();
}

fn dump() {
}

fn usage(whoami: &str) {
    println!("usage: {} --add[-blocking] path", whoami);
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

fn do_add(data_file: &path::PathBuf, what: &str) -> io::Result<()> {
    let mut table: vec::Vec<Row> = vec::Vec::with_capacity(400);
    {
        let fd = try!(fs::File::open(data_file));
        let reader = io::BufReader::new(&fd);
        let mut count: f32 = 0.0;
        let mut updated: bool = false;

        for line in reader.lines() {
            let the_line = line.unwrap();
            let parsed = to_row(&the_line);
            if parsed.is_err() {
                continue;
            }
            let mut row = parsed.unwrap();

            // if something has stopped being a directory, drop it
            if !path::Path::new(&row.path).is_dir() {
                continue
            }

            // if we've found the thing we were going to add, update it instead
            if !updated && row.path == what {
                row.rank += 1.0;
                row.time = unix_time();
                updated = true;
            }

            count += row.rank;

            table.push(row);
        }

        // if we didn't find the thing to add, add it now
        if !updated {
            table.push(Row { path: String::from(what), rank: 1.0, time: unix_time() });
        }

        // aging
        if count > 9000.0 {
            for line in &mut table {
                line.rank *= 0.99;
            }
        }
    }

    let tmp = tempfile::NamedTempFile::new_in(data_file.parent().unwrap())
        .expect("couldn't make a temporary file near data file");

    {
       let of = fs::File::create(tmp.path()).unwrap();

        let mut writer = io::BufWriter::new(&of);
        for line in table {
            try!(write!(writer, "{}|{}|{}\n", line.path, line.rank, line.time));
        }
    }

    try!(tmp.persist(data_file));

    return Ok(());
}

fn coded_main() -> u8 {
    let mut args = env::args();
    let arg_count = args.len();
    if arg_count <= 1 {
        dump();
        return 0;
    }

    let whoami = args.next().unwrap();
    let command = args.next().unwrap();
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

    let data_file = match env::var("_Z_DATA") {
        Ok(x) => path::PathBuf::from(&x),
        Err(_) => {
            let home = env::home_dir().expect("home directory must be locatable");
            home.join(".z")
        },
    };

    if "--add-blocking" == command {
        if 3 != arg_count {
            usage(&whoami);
            return 2;
        }

        let err = do_add(&data_file, &args.next().unwrap());
        err.unwrap();
        return 0;
    }

    thread::sleep(time::Duration::from_millis(200));
    println!("Hello, world!");
    return 1;
}

fn main() {
    std::process::exit(coded_main() as i32);
}
