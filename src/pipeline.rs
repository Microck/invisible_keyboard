//! Causal stream segmentation around the inspectable baseline decoder.

use std::{collections::VecDeque, error::Error, fmt};

use crate::{
    decoder::{AbstainReason, BorrowedTapWindow, Decision, DecisionKind, HandDecoder, WindowError},
    layout::Hand,
    protocol::SensorFrame,
};

/// Fixed causal context consumed by the baseline decoder at 200 Hz.
pub const WINDOW_FRAMES: usize = 16;

/// Post-trigger samples collected before committing one decision.
pub const DECISION_DELAY_FRAMES: u8 = 3;

/// Consecutive idle windows required before another tap can fire.
pub const REARM_IDLE_WINDOWS: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
enum PipelineState {
    Ready,
    Pending { remaining: u8, candidate: Decision },
    Suppressed { idle_windows: u8 },
}

/// A stream violation that resets all segmentation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipelineError {
    WrongHand { expected: Hand, actual: Hand },
    InvalidWindow(WindowError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongHand { expected, actual } => {
                write!(
                    formatter,
                    "pipeline expected {expected:?} but received {actual:?}"
                )
            }
            Self::InvalidWindow(error) => write!(formatter, "pipeline stream is invalid: {error}"),
        }
    }
}

impl Error for PipelineError {}

/// One-hand rolling causal segmenter.
///
/// A candidate starts a fixed three-frame decision delay. The newest active
/// classification replaces the prior candidate during that delay. After one
/// result, the pipeline suppresses repeats until four consecutive rolling
/// windows contain no intent. The result timestamp records the commit frame
/// even when an idle window retains an earlier classification. These constants
/// are software baselines, not validated human timing thresholds.
#[derive(Clone, Debug)]
pub struct CausalPipeline {
    decoder: HandDecoder,
    frames: VecDeque<SensorFrame>,
    state: PipelineState,
}

impl CausalPipeline {
    /// Construct an empty stream pipeline around one personal decoder.
    #[must_use]
    pub fn new(decoder: HandDecoder) -> Self {
        Self {
            decoder,
            frames: VecDeque::with_capacity(WINDOW_FRAMES),
            state: PipelineState::Ready,
        }
    }

    /// Add one aggregate frame and optionally emit one debounced decision.
    ///
    /// # Errors
    ///
    /// A wrong-hand frame, device-time regression, or per-sensor sample-time
    /// regression clears the rolling state and returns [`PipelineError`].
    pub fn push(&mut self, frame: SensorFrame) -> Result<Option<Decision>, PipelineError> {
        if frame.hand != self.decoder.hand() {
            let error = PipelineError::WrongHand {
                expected: self.decoder.hand(),
                actual: frame.hand,
            };
            self.reset();
            return Err(error);
        }

        if self.frames.len() == WINDOW_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
        let (first, second) = self.frames.as_slices();
        let window = match BorrowedTapWindow::new(first, second) {
            Ok(window) => window,
            Err(error) => {
                self.reset();
                return Err(PipelineError::InvalidWindow(error));
            }
        };

        if self.frames.len() < WINDOW_FRAMES {
            return Ok(None);
        }

        let decision = self.decoder.decode_borrowed(window);
        let state = self.state;
        let emitted = match state {
            PipelineState::Ready if is_no_intent(decision) => None,
            PipelineState::Ready if is_quality_fault(decision) => {
                self.state = PipelineState::Suppressed { idle_windows: 0 };
                Some(decision)
            }
            PipelineState::Ready => {
                self.state = PipelineState::Pending {
                    remaining: DECISION_DELAY_FRAMES,
                    candidate: decision,
                };
                None
            }
            PipelineState::Pending { .. } if is_quality_fault(decision) => {
                self.state = PipelineState::Suppressed { idle_windows: 0 };
                Some(decision)
            }
            PipelineState::Pending {
                remaining,
                candidate,
            } => {
                let mut candidate = if is_no_intent(decision) {
                    candidate
                } else {
                    decision
                };
                if remaining <= 1 {
                    self.state = PipelineState::Suppressed { idle_windows: 0 };
                    candidate.decision_time_us = decision.decision_time_us;
                    Some(candidate)
                } else {
                    self.state = PipelineState::Pending {
                        remaining: remaining - 1,
                        candidate,
                    };
                    None
                }
            }
            PipelineState::Suppressed { .. } if is_quality_fault(decision) => {
                self.state = PipelineState::Suppressed { idle_windows: 0 };
                Some(decision)
            }
            PipelineState::Suppressed { idle_windows } if is_no_intent(decision) => {
                let idle_windows = idle_windows.saturating_add(1);
                self.state = if idle_windows >= REARM_IDLE_WINDOWS {
                    PipelineState::Ready
                } else {
                    PipelineState::Suppressed { idle_windows }
                };
                None
            }
            PipelineState::Suppressed { .. } => {
                self.state = PipelineState::Suppressed { idle_windows: 0 };
                None
            }
        };
        Ok(emitted)
    }

    /// Return how many frames currently contribute to the rolling window.
    #[must_use]
    pub fn buffered_frames(&self) -> usize {
        self.frames.len()
    }

    /// Clear causal context and return to an empty ready state.
    pub fn reset(&mut self) {
        self.frames.clear();
        self.state = PipelineState::Ready;
    }
}

fn is_no_intent(decision: Decision) -> bool {
    decision.kind == DecisionKind::Abstain(AbstainReason::NoIntent)
}

fn is_quality_fault(decision: Decision) -> bool {
    matches!(
        decision.kind,
        DecisionKind::Abstain(reason) if reason.invalidates_live_output()
    )
}
