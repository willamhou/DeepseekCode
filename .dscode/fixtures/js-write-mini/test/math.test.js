import test from "node:test";
import assert from "node:assert/strict";

import { add } from "../src/math.js";

test("add returns the sum", () => {
  assert.equal(add(2, 3), 5);
});
