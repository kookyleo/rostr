use clap::Command;

fn main() -> std::io::Result<()> {
    let mut command = Command::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .after_help("Initial CLI skeleton. Model discovery and selection are not yet implemented.");

    command.get_matches_mut();
    command.print_help()
}
