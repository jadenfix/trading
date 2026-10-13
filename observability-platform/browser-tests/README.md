# Paper dashboard browser gate

This drives the actual local dashboard, broker and trace API. It verifies labels,
authenticated approval, soft-cancel request state, backend state and reload
persistence. It uses synthetic paper fixtures and does not start a worker or
place exchange orders. A soft cancel remains requested until a worker consumes
it; this gate does not incorrectly label that request as completed cancellation.

```bash
cd observability-platform/browser-tests
npm install
npx playwright-core install chromium
npm test
```

To use a separately installed compatible Chromium, set `BROWSER_EXECUTABLE`.
Set `PLAYWRIGHT_PACKAGE` to an absolute installed package path when reusing an
existing runtime. `BROWSER_EVIDENCE_PATH` optionally captures a local QA image.
The default run does not write screenshots. Browser and backend subprocesses
are stopped in `finally`, including assertion failures.

Successful browser checks establish this paper control-plane workflow only.
Rust workers, exchange integrations, GPU/model execution and deployment require
separate receipts. Coordinate recording using
https://github.com/jadenfix/jadenfix/issues/1 and revalidate the current head.
