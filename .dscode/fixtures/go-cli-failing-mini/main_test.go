package gocli

import "testing"

func TestRouteBenchmarkCommand(t *testing.T) {
	if got := RouteBenchmarkCommand("bench"); got != "run benchmark" {
		t.Fatalf("RouteBenchmarkCommand(%q) = %q, want %q", "bench", got, "run benchmark")
	}
}

func TestMainDefaultsToBench(t *testing.T) {
	if got := Main(nil); got != "run benchmark" {
		t.Fatalf("Main(nil) = %q, want %q", got, "run benchmark")
	}
}
