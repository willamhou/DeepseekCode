package gocli

func RouteBenchmarkCommand(name string) string {
	if name == "bench" {
		return "run bench"
	}
	if name == "doctor" {
		return "show diagnostics"
	}
	return "unknown command"
}

func Main(args []string) string {
	command := "bench"
	if len(args) > 0 {
		command = args[0]
	}
	return RouteBenchmarkCommand(command)
}
