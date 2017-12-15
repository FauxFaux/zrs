error_chain! {
    foreign_links {
        ParseFloatError(::std::num::ParseFloatError);
        ParseIntError(::std::num::ParseIntError);
        Io(::std::io::Error);
        TempPersistError(::tempfile::PersistError);
        Regex(::regex::Error);
    }
}
