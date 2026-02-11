fn main() {
    use clap::Parser;
    use std::error::Error;
    let args = rdrscrape::cli::Args::parse();
    if let Err(e) = rdrscrape::cli::run(&args) {
        eprintln!("{}", e);
        if args.verbose {
            let mut source = e.source();
            while let Some(s) = source {
                eprintln!("  cause: {}", s);
                source = s.source();
            }
        }
        std::process::exit(e.exit_code());
    }
}
