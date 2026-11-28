import { test } from "node:test";
import assert from "node:assert/strict";
import { total } from "./total.js";
// node's own runner: this fixture needs nothing installed to be green
test("totals a basket", () => assert.equal(total([{ price: 2, qty: 3 }, { price: 1, qty: 1 }]), 7));
test("an empty basket is zero", () => assert.equal(total([]), 0));
