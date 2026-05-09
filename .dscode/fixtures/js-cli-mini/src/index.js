export function routeBenchmarkCommand(name) {
  if (name === "bench") {
    return "run benchmark";
  }
  if (name === "doctor") {
    return "show diagnostics";
  }
  return "unknown command";
}

export function main(argv = []) {
  const command = argv[0] ?? "bench";
  return routeBenchmarkCommand(command);
}
