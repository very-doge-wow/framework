use clap::Parser;

fn main() -> anyhow::Result<()> {
    framework_cli::run(framework_cli::Cli::parse(), &mut std::io::stdout())
}
