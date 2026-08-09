const token = document.querySelector('meta[name="calibration-token"]').content;
const EVIDENCE_HARDWARE = 'hardware';
const EVIDENCE_SIMULATED_REPLAY = 'simulated-replay';
const ACTIVE_CAPTURE_POLL_MS = 100;
const INACTIVE_POLL_MS = 1000;
const VALIDATION_PASSED_COPY = 'Held-out validation passed. The model and evidence were saved. Saved files do not prove that the wearables are still in the same position.';
const STOPPED_COPY = 'Typing stopped and output is neutral. This wear-session confirmation is no longer active. Confirm again before starting a new live process. Revalidate first if either wearable moved.';
const FAILURE_COPY = Object.freeze({
  'hardware-fault': 'Calibration failed because the sensor stream or timing became invalid. Check both wearables, then start a new calibration.',
  'unsupported-layout': 'Calibration stopped because this setup supports only US QWERTY physical positions. Use a matching keyboard or stop.',
  'heldout-validation-failed': 'Held-out validation failed the fixed key and no-key checks. Check the wearable position and surface, then start a new calibration.',
  'validation-plateau': 'Calibration failed because fresh evidence did not improve the fixed signal check. Reseat the wearables, then start a new calibration.',
  'example-cap': 'Calibration reached the per-position sensor-evidence cap. Check the named hand and finger ownership, then start a new calibration.',
  'time-cap': 'Calibration stopped at the 20-minute fail-closed cap. No model was accepted. Check the wearables, then start a new calibration.',
});
const screens = [...document.querySelectorAll('[data-screen]')];
const details = document.querySelector('#engineering-details');
const evidenceBanner = document.querySelector('#evidence-banner');
const advanceButton = document.querySelector('#advance-demo');
const pauseButton = document.querySelector('#pause');
const resumeButton = document.querySelector('#resume');
const abortButton = document.querySelector('#abort');
const acknowledgeResultButton = document.querySelector('#acknowledge-result');
const surfaceTransferScreen = document.querySelector('#screen-surface-transfer');
const surfacePrompt = document.querySelector('#surface-prompt');
const layoutDecisionButtons = [...document.querySelectorAll('[data-layout-decision]')];
const surfacePositions = [...document.querySelectorAll('[data-position]')];
let renderedRevision = -1;
let captureLeaseRevision = null;
let layoutInstructionRevision = null;
let resultAcknowledgementRevision = null;
let pendingSurfaceInstructionRevision = null;
let surfaceInstructionAcknowledgementInFlightRevision = null;
let lastAcknowledgedSurfaceInstructionRevision = null;
let refreshStopped = false;
let heartbeatInFlight = false;
let refreshTimer = null;

async function api(path, options = {}) {
  const response = await fetch(path, {
    cache: 'no-store',
    credentials: 'same-origin',
    ...options,
    headers: {
      'X-Calibration-Token': token,
      ...(options.headers || {}),
    },
  });
  if (!response.ok) {
    throw new Error(`Local service returned ${response.status}`);
  }
  return response.json();
}

function setText(selector, value) {
  const element = document.querySelector(selector);
  if (element) element.textContent = value;
}

function applyState(state) {
  if (state.revision < renderedRevision) return;
  // Receipt acceptance deliberately does not create a second instruction
  // revision. Retry that non-rendering side effect, but leave an identical UI
  // snapshot alone.
  if (state.revision === renderedRevision) {
    acknowledgeSurfaceInstructionAfterPaint(state);
    return;
  }
  render(state);
}

function render(state) {
  layoutInstructionRevision = state.keyboard_layout_instruction_revision;
  resultAcknowledgementRevision = state.result_acknowledgement_revision;
  // `null` is authoritative: the service has no receipt-bound instruction.
  // Clear client retry state even if an earlier acknowledgement response was
  // lost, because the runner may already have consumed that receipt.
  if (state.surface_instruction_revision === null) {
    pendingSurfaceInstructionRevision = null;
  } else if (state.surface_instruction_revision !== lastAcknowledgedSurfaceInstructionRevision) {
    pendingSurfaceInstructionRevision = state.surface_instruction_revision;
  }
  if (state.capture_lease_revision === null || state.evidence !== EVIDENCE_HARDWARE) {
    pendingSurfaceInstructionRevision = null;
  }
  renderedRevision = state.revision;
  captureLeaseRevision = state.capture_lease_revision;
  for (const screen of screens) {
    screen.hidden = screen.dataset.screen !== state.screen;
  }

  const percent = Math.floor(state.setup_progress_basis_points / 100);
  const progress = document.querySelector('#setup-progress');
  progress.value = state.setup_progress_basis_points;
  setText('#progress-label', `${percent} percent`);
  setText('#hardware-health', state.hardware_health);
  setText('#keyboard-device', state.keyboard_device);
  setText('#supported-keyboard-layout', state.supported_keyboard_layout);
  setText('#keyboard-layout-confirmation', {
    'waiting-for-device': 'Waiting for device selection',
    'awaiting-user': 'Waiting for your answer',
    confirmed: 'Confirmed',
    rejected: 'Unsupported layout',
    'simulated-fixture': 'Simulated fixture only',
  }[state.keyboard_layout_confirmation] || state.keyboard_layout_confirmation);
  setText('#neutral-seconds', String(state.neutral_seconds));
  setText('#elapsed-seconds', String(state.elapsed_seconds));
  setText('#keyboard-prompt', state.prompt);
  setText('#keyboard-coverage', `${state.keyboard_accepted} of ${state.keyboard_required}`);
  setText('#keyboard-precheck', state.keyboard_precheck);
  setText('#keyboard-accuracy', state.keyboard_accuracy);
  setText('#surface-coverage', `${state.surface_accepted} of ${state.surface_required}`);
  setText('#rejected-count', String(state.rejected_examples));
  setText('#surface-prompt', state.surface_target);
  setText('#surface-stage', {
    'surface-validate': 'Held-out validation is active. Follow each cue. These targets cannot change the locked model.',
    'fit-locked-artifact': 'The model is being locked. Keep this page visible and do not move either wearable.',
  }[state.phase] || 'Passive-surface fitting is active. Follow each cue on the plain surface.');
  for (const position of surfacePositions) {
    const active = position.dataset.position === state.surface_position;
    position.classList.toggle('active', active);
    if (active) position.setAttribute('aria-current', 'true');
    else position.removeAttribute('aria-current');
  }
  acknowledgeSurfaceInstructionAfterPaint(state);
  renderValidationResult(state);
  setText('#recording-state', serviceStatus(state));

  evidenceBanner.textContent = `${state.evidence}: ${state.limitation}`;
  evidenceBanner.dataset.mode = state.evidence;
  const terminal = state.screen === 'validation-result';
  advanceButton.hidden = state.evidence !== EVIDENCE_SIMULATED_REPLAY || terminal;
  const hardwareCapture = state.evidence === EVIDENCE_HARDWARE;
  const layoutDecisionPending = hardwareCapture
    && state.phase === 'keyboard-identify'
    && state.keyboard_layout_confirmation === 'awaiting-user';
  for (const button of layoutDecisionButtons) button.hidden = !layoutDecisionPending;
  // Physical capture is one fail-closed attempt. A pause would split the raw
  // timeline and invalidate its timing and exposure claims, so hardware mode
  // offers one Start action and requires a restart after any lease loss.
  pauseButton.hidden = hardwareCapture || state.paused || terminal;
  resumeButton.hidden = hardwareCapture || !state.paused || terminal;
  abortButton.hidden = terminal || state.result_acknowledgement !== 'unavailable';
  acknowledgeResultButton.hidden = state.result_acknowledgement !== 'pending';
  if (state.result_acknowledgement === 'acknowledged') refreshStopped = true;
  resumeButton.textContent = 'Resume';
  details.textContent = JSON.stringify(state, null, 2);
}

function serviceStatus(state) {
  if (state.result_acknowledgement === 'pending') return 'Saved result ready for review';
  if (state.result_acknowledgement === 'acknowledged') return 'Saved result acknowledged';
  if (state.phase === 'validation-passed') return 'Saving the validated result';
  if (state.phase === 'calibration-failed') return 'Calibration failed';
  return state.paused ? 'Setup paused' : 'Local capture active';
}

function renderValidationResult(state) {
  const failureElement = document.querySelector('#validation-cause');
  if (state.heldout_validation_passed) {
    const filesSaved = state.result_acknowledgement !== 'unavailable';
    setText(
      '#validation-status',
      filesSaved ? VALIDATION_PASSED_COPY : 'Held-out validation passed. Saving the model and evidence.',
    );
    failureElement.textContent = '';
  } else if (state.phase === 'calibration-failed') {
    setText('#validation-status', 'Calibration failed. No validated model is available.');
    failureElement.textContent = FAILURE_COPY[state.failure_cause]
      || 'Calibration failed with an unknown structured cause. Check engineering details before retrying.';
  } else if (state.phase === 'aborted') {
    setText('#validation-status', STOPPED_COPY);
    failureElement.textContent = '';
  } else if (state.validation === 'running') {
    setText('#validation-status', 'Held-out validation is running on the locked model.');
    failureElement.textContent = '';
  } else {
    setText('#validation-status', 'Held-out validation has not run.');
    failureElement.textContent = '';
  }

  setText('#result-acknowledgement-status', {
    unavailable: state.heldout_validation_passed
      ? 'Saving the final result before acknowledgement is enabled.'
      : 'The saved result is not ready for acknowledgement.',
    pending: 'Review the result, then acknowledge it. This page remains available for up to five minutes.',
    acknowledged: 'Saved result acknowledged. You can close this page.',
  }[state.result_acknowledgement] || 'Saved result acknowledgement state is unavailable.');
}

async function postAction(action, revision, keepalive = false) {
  return api('/api/action', {
    method: 'POST',
    keepalive,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ action, revision }),
  });
}

function acknowledgeSurfaceInstructionAfterPaint(state) {
  const instructionRevision = pendingSurfaceInstructionRevision;
  if (instructionRevision === null
      || instructionRevision === lastAcknowledgedSurfaceInstructionRevision
      || surfaceInstructionAcknowledgementInFlightRevision !== null
      || state.evidence !== EVIDENCE_HARDWARE
      || state.screen !== 'surface-transfer') {
    return;
  }
  surfaceInstructionAcknowledgementInFlightRevision = instructionRevision;

  // Two rendering frames place the receipt after the cue has reached the DOM
  // and the browser has had an opportunity to paint it. Keep the revision
  // pending until the local service confirms receipt so a lost response can
  // retry the same idempotent acknowledgement while the lease remains live.
  // This receipt opens only an intent/time gate. It is never sensor contact.
  window.requestAnimationFrame(() => {
    window.requestAnimationFrame(() => {
      if (document.visibilityState !== 'visible'
          || captureLeaseRevision === null
          || pendingSurfaceInstructionRevision !== instructionRevision
          || surfaceTransferScreen.hidden
          || surfacePrompt.textContent !== state.surface_target) {
        finishSurfaceInstructionAcknowledgement(instructionRevision);
        return;
      }
      void postAction('surface-instruction-rendered', instructionRevision, true)
        .then((nextState) => {
          lastAcknowledgedSurfaceInstructionRevision = instructionRevision;
          if (pendingSurfaceInstructionRevision === instructionRevision) {
            pendingSurfaceInstructionRevision = null;
          }
          finishSurfaceInstructionAcknowledgement(instructionRevision);
          applyState(nextState);
        })
        .catch((error) => {
          finishSurfaceInstructionAcknowledgement(instructionRevision);
          setText('#recording-state', error.message);
        });
    });
  });
}

function finishSurfaceInstructionAcknowledgement(instructionRevision) {
  if (surfaceInstructionAcknowledgementInFlightRevision === instructionRevision) {
    surfaceInstructionAcknowledgementInFlightRevision = null;
  }
}

function stopCapture(action) {
  if (captureLeaseRevision === null) return;
  const expiringLease = captureLeaseRevision;
  captureLeaseRevision = null;
  void postAction(action, expiringLease, true).catch(() => {
    // Browser lifecycle signals are advisory. The local service watchdog is
    // authoritative and will expire the same lease if this request is lost.
  });
}

async function refresh() {
  try {
    applyState(await api('/api/state'));
  } catch (error) {
    setText('#recording-state', error.message);
  }
}

for (const button of document.querySelectorAll('[data-action]')) {
  button.addEventListener('click', async () => {
    const layoutDecision = button.hasAttribute('data-layout-decision');
    const resultAcknowledgement = button.hasAttribute('data-result-acknowledgement');
    const actionRevision = layoutDecision
      ? layoutInstructionRevision
      : resultAcknowledgement
        ? resultAcknowledgementRevision
        : renderedRevision;
    if (actionRevision === null) return;
    const disabledButtons = layoutDecision ? layoutDecisionButtons : [button];
    for (const current of disabledButtons) current.disabled = true;
    try {
      const state = await postAction(button.dataset.action, actionRevision);
      applyState(state);
      scheduleRefresh();
    } catch (error) {
      setText('#recording-state', error.message);
    } finally {
      for (const current of disabledButtons) current.disabled = false;
    }
  });
}

async function refreshLoop() {
  if (refreshStopped) return;
  await refresh();
  scheduleRefresh();
}

function scheduleRefresh() {
  if (refreshTimer !== null) window.clearTimeout(refreshTimer);
  refreshTimer = null;
  if (refreshStopped) return;

  const delay = captureLeaseRevision === null
    ? INACTIVE_POLL_MS
    : ACTIVE_CAPTURE_POLL_MS;
  refreshTimer = window.setTimeout(() => {
    refreshTimer = null;
    void refreshLoop();
  }, delay);
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') stopCapture('visibility-hidden');
});
window.addEventListener('blur', () => stopCapture('blur'));
window.addEventListener('pagehide', () => stopCapture('pagehide'));
window.addEventListener('beforeunload', () => stopCapture('beforeunload'));
window.addEventListener('offline', () => stopCapture('offline'));

document.addEventListener('keydown', (event) => {
  const focusedControl = event.target.closest('a, button, input, select, summary, textarea');
  const pageNavigationKey = event.code === 'Space' || event.key === 'Backspace';
  if (captureLeaseRevision !== null && pageNavigationKey && !focusedControl) {
    // The selected evdev source remains authoritative. Suppress only the
    // page navigation caused by Space or Backspace. Tab, Enter, and control
    // activation remain available to keyboard users.
    event.preventDefault();
  }
});

window.setInterval(() => {
  if (captureLeaseRevision === null || heartbeatInFlight) return;
  heartbeatInFlight = true;
  void postAction('heartbeat', captureLeaseRevision, true)
    .then(applyState)
    .catch((error) => setText('#recording-state', error.message))
    .finally(() => { heartbeatInFlight = false; });
}, 1000);

void refreshLoop();
