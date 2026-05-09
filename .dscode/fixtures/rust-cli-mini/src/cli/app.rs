pub fn cli_from_argv(args: &[String]) -> &'static str {
    match args.first().map(String::as_str) {
        Some("benchmark") => route_benchmark_subcommand(),
        Some("doctor") => "doctor",
        Some("diff") => "diff",
        _ => "chat",
    }
}

fn route_benchmark_subcommand() -> &'static str {
    "benchmark"
}
