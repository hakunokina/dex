import { test } from "node:test";
import assert from "node:assert/strict";
import { gatewayError } from "./gatewayView.ts";

test("gateway auth errors distinguish login, quota and access without reflecting secrets", () => {
  assert.match(gatewayError("gateway_login_required"), /Giriş/);
  assert.match(gatewayError(new Error("gateway_auth_rate_limited")), /Kota/);
  assert.match(gatewayError("gateway_account_ineligible"), /erişim/);
  assert.doesNotMatch(gatewayError("Bearer SECRET-CANARY"), /SECRET|Bearer/);
});
