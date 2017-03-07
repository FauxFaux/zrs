use std::env;
use std::thread;
use std::time;
use std::process::Command;

fn dump() {
}

fn usage(whoami: &str) {
    println!("usage: {} --add[-blocking] path", whoami);
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
        if arg_count != 3 {
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
    thread::sleep(time::Duration::from_millis(200));
    println!("Hello, world!");
    return 1;
}

fn main() {
    std::process::exit(coded_main() as i32);
}
