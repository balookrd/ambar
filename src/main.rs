use clap::Parser;

#[derive(Parser)]
#[command(name = "ambar", version, about = "Replicated volumes for k3s on top of DRBD 9")]
struct Cli {}

fn main() {
    Cli::parse();
}
