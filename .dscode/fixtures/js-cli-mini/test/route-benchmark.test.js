import test from "node:test";
import assert from "node:assert/strict";

import { main, routeBenchmarkCommand } from "../src/index.js";

test("routeBenchmarkCommand routes bench", () => {
  assert.equal(routeBenchmarkCommand("bench"), "run benchmark");
});

test("main defaults to bench", () => {
  assert.equal(main([]), "run benchmark");
});
