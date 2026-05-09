mod cli;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let route = cli::app::cli_from_argv(&args);
    println!("{route}");
}
