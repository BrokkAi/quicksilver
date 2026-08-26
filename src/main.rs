fn main() {
    let cli = cargo_quicksilver::cli::parse_cli();
    match cargo_quicksilver::run_cli(cli) {
        Ok(code) => {
            if code != 0 {
                std::process::exit(code);
            }
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(2);
        }
    }
}
