def route_benchmark_command(name: str) -> str:
    if name == "bench":
        return "run benchmark"
    if name == "doctor":
        return "show diagnostics"
    return "unknown command"


def main(argv: list[str] | None = None) -> str:
    args = argv or []
    command = args[0] if args else "bench"
    return route_benchmark_command(command)
