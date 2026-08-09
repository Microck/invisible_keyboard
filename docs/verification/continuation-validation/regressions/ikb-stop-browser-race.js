async ({ tab, page }, options) => {
  const cdp = await page.target().createCDPSession();
  let resolveRequest, resolveResponse, abortNetworkId;
  const requestReady = new Promise((resolve) => {
    resolveRequest = resolve;
  });
  const responseReady = new Promise((resolve) => {
    resolveResponse = resolve;
  });
  cdp.on("Fetch.requestPaused", (event) => {
    const body = event.request.postData;
    if (event.responseStatusCode === undefined && body && JSON.parse(body).action === "abort") {
      abortNetworkId = event.networkId;
      resolveRequest(event);
    } else if (event.responseStatusCode !== undefined && event.networkId === abortNetworkId) {
      resolveResponse(event);
    } else {
      cdp.send("Fetch.continueRequest", { requestId: event.requestId });
    }
  });
  await cdp.send("Fetch.enable", { patterns: [
    { urlPattern: "*/api/action", requestStage: "Request" },
    { urlPattern: "*/api/action", requestStage: "Response" }
  ] });
  try {
    const clicking = tab.click("#abort");
    const held = await requestReady;
    const posted = JSON.parse(held.request.postData);
    await tool.write({ i: "Advancing real setup progress", path: options.fixture, content: options.command });
    await page.waitForFunction(async (revision) => {
      const token = document.querySelector('meta[name="calibration-token"]').content;
      const state = await fetch("/api/state", { headers: { "X-Calibration-Token": token } }).then((response) => response.json());
      return state.revision > revision;
    }, { timeout: 1e4 }, posted.revision);
    await cdp.send("Fetch.continueRequest", { requestId: held.requestId });
    const response = await responseReady;
    const captured = await cdp.send("Fetch.getResponseBody", { requestId: response.requestId });
    const responseBody = captured.base64Encoded ? Buffer.from(captured.body, "base64").toString("utf8") : captured.body;
    await cdp.send("Fetch.continueRequest", { requestId: response.requestId });
    await clicking;
    await page.waitForFunction(() => !document.querySelector("#abort").disabled);
    return await page.evaluate(async (observation) => {
      const token = document.querySelector('meta[name="calibration-token"]').content;
      const state = await fetch("/api/state", { headers: { "X-Calibration-Token": token } }).then((response) => response.json());
      if (state.revision <= observation.postedRevision || observation.status !== observation.expectedStatus)
        throw new Error("Stop did not cross the expected revision/status boundary");
      if (observation.expectedStatus === 200 && (state.phase !== "aborted" || !state.paused || state.capture_lease_revision !== null))
        throw new Error("Successful Stop did not revoke capture");
      if (observation.expectedStatus === 400 && (state.phase !== "surface-validate" || state.paused || state.capture_lease_revision === null))
        throw new Error("Baseline rejection did not preserve active capture");
      return { ...observation, currentRevision: state.revision, phase: state.phase, paused: state.paused, captureLease: state.capture_lease_revision, recordingState: document.querySelector("#recording-state").textContent };
    }, { postedRevision: posted.revision, status: response.responseStatusCode, responseBody, expectedStatus: options.expectedStatus });
  } finally {
    await cdp.send("Fetch.disable");
    await cdp.detach();
  }
}
