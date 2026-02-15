import test from "node:test";
import assert from "node:assert/strict";
import { buildApiConfigResponse } from "../trace-api/config-response.mjs";

test("buildApiConfigResponse never exposes a control token", () => {
  const config = buildApiConfigResponse({
    brokerBaseUrl: "http://127.0.0.1:8787",
    temporalUiUrl: "http://127.0.0.1:8233",
    traceApiUrl: "http://127.0.0.1:8791",
    defaultProject: "local",
    defaultLocation: "us-central1",
    controlToken: "local-dev-token",
  });

  assert.equal(config.control_token_required, true);
  assert.equal(config.control_token_default, null);
});
