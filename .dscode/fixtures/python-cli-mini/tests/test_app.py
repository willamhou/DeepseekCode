from app import main, route_benchmark_command


def test_route_benchmark_command():
    assert route_benchmark_command("bench") == "run benchmark"


def test_main_defaults_to_bench():
    assert main([]) == "run benchmark"
