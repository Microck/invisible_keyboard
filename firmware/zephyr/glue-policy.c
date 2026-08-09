#include "glue-policy.h"

#include <limits.h>
#include <string.h>

void ikb_glue_sensor_initialize(ikb_glue_sensor_policy_t *sensor) {
  if (sensor != NULL) {
    memset(sensor, 0, sizeof(*sensor));
    sensor->state = IKB_GLUE_SENSOR_IDLE;
  }
}

ikb_glue_sensor_state_t ikb_glue_sensor_state(
    const ikb_glue_sensor_policy_t *sensor) {
  return sensor == NULL ? IKB_GLUE_SENSOR_CLOSED : sensor->state;
}

bool ikb_glue_sensor_admit(ikb_glue_sensor_policy_t *sensor,
                           uint64_t edge_time_us) {
  if (sensor == NULL || sensor->state != IKB_GLUE_SENSOR_IDLE) {
    return false;
  }
  sensor->edge_time_us = edge_time_us;
  sensor->state = IKB_GLUE_SENSOR_ADMITTED_UNSUBMITTED;
  return true;
}

bool ikb_glue_sensor_mark_submitted(ikb_glue_sensor_policy_t *sensor) {
  if (sensor == NULL ||
      sensor->state != IKB_GLUE_SENSOR_ADMITTED_UNSUBMITTED) {
    return false;
  }
  sensor->state = IKB_GLUE_SENSOR_SUBMITTED;
  return true;
}

bool ikb_glue_sensor_abort_submission(ikb_glue_sensor_policy_t *sensor) {
  if (sensor == NULL ||
      sensor->state != IKB_GLUE_SENSOR_ADMITTED_UNSUBMITTED) {
    return false;
  }
  sensor->state = IKB_GLUE_SENSOR_IDLE;
  return true;
}

bool ikb_glue_sensor_begin(ikb_glue_sensor_policy_t *sensor,
                           uint64_t *edge_time_us) {
  if (sensor == NULL || edge_time_us == NULL ||
      sensor->state != IKB_GLUE_SENSOR_SUBMITTED) {
    return false;
  }
  *edge_time_us = sensor->edge_time_us;
  sensor->state = IKB_GLUE_SENSOR_RUNNING;
  return true;
}

bool ikb_glue_sensor_pending_edge(const ikb_glue_sensor_policy_t *sensor,
                                  uint64_t *edge_time_us) {
  if (sensor == NULL || edge_time_us == NULL ||
      sensor->state == IKB_GLUE_SENSOR_IDLE ||
      sensor->state == IKB_GLUE_SENSOR_CLOSED) {
    return false;
  }
  *edge_time_us = sensor->edge_time_us;
  return true;
}

bool ikb_glue_sensor_complete(ikb_glue_sensor_policy_t *sensor) {
  if (sensor == NULL || sensor->state != IKB_GLUE_SENSOR_RUNNING) {
    return false;
  }
  sensor->state = IKB_GLUE_SENSOR_IDLE;
  return true;
}

bool ikb_glue_sensor_close(ikb_glue_sensor_policy_t *sensor) {
  if (sensor == NULL || sensor->state != IKB_GLUE_SENSOR_IDLE) {
    return false;
  }
  sensor->state = IKB_GLUE_SENSOR_CLOSED;
  return true;
}

void ikb_glue_policy_initialize(ikb_glue_policy_t *policy) {
  if (policy != NULL) {
    memset(policy, 0, sizeof(*policy));
    policy->lifecycle = IKB_GLUE_RUNNING;
  }
}

void ikb_glue_policy_fail(ikb_glue_policy_t *policy) {
  if (policy == NULL) {
    return;
  }
  policy->failure_latched = true;
  policy->boundary_armed = false;
  if (policy->lifecycle >= IKB_GLUE_BOUNDARY_CLOSED) {
    ikb_transport_slot_drop(
        &policy->transport[IKB_GLUE_TRANSPORT_RECEIPT]);
  }
}

bool ikb_glue_policy_failed(const ikb_glue_policy_t *policy) {
  return policy == NULL || policy->failure_latched;
}

ikb_glue_lifecycle_t ikb_glue_policy_lifecycle(
    const ikb_glue_policy_t *policy) {
  return policy == NULL ? IKB_GLUE_QUIESCED : policy->lifecycle;
}

bool ikb_glue_admission_open(const ikb_glue_policy_t *policy) {
  return policy != NULL && policy->lifecycle <= IKB_GLUE_STOP_REQUESTED;
}

static bool identity_matches(
    const ikb_quiesce_request_t *request, ikb_hand_t expected_hand,
    const uint8_t expected_controller_id[IKB_CONTROLLER_ID_SHA256_LEN],
    const uint8_t expected_boot_id[IKB_BOOT_ID_LEN]) {
  return request != NULL && expected_controller_id != NULL &&
         expected_boot_id != NULL && request->hand == expected_hand &&
         memcmp(request->controller_id_sha256, expected_controller_id,
                IKB_CONTROLLER_ID_SHA256_LEN) == 0 &&
         memcmp(request->boot_id, expected_boot_id, IKB_BOOT_ID_LEN) == 0 &&
         ikb_has_nonzero_byte(request->session_id,
                              sizeof(request->session_id));
}

bool ikb_glue_request_quiesce(
    ikb_glue_policy_t *policy, const ikb_quiesce_request_t *request,
    ikb_hand_t expected_hand,
    const uint8_t expected_controller_id[IKB_CONTROLLER_ID_SHA256_LEN],
    const uint8_t expected_boot_id[IKB_BOOT_ID_LEN], uint64_t accepted_time_us) {
  if (policy == NULL) {
    return false;
  }
  if (policy->failure_latched || policy->lifecycle != IKB_GLUE_RUNNING ||
      !identity_matches(request, expected_hand, expected_controller_id,
                        expected_boot_id)) {
    ikb_glue_policy_fail(policy);
    return false;
  }

  policy->request = *request;
  policy->request_accepted_time_us = accepted_time_us;
  policy->lifecycle = IKB_GLUE_STOP_REQUESTED;
  return true;
}

bool ikb_glue_quiesce_request(const ikb_glue_policy_t *policy,
                              ikb_quiesce_request_t *request) {
  if (policy == NULL || request == NULL ||
      policy->lifecycle == IKB_GLUE_RUNNING) {
    return false;
  }
  *request = policy->request;
  return true;
}

uint64_t ikb_glue_next_aggregate_tick(uint64_t *target_time_us,
                                      uint32_t ticks_per_second) {
  *target_time_us += IKB_AGGREGATE_PERIOD_US;
  const uint64_t seconds = *target_time_us / 1000000U;
  const uint64_t remainder_us = *target_time_us % 1000000U;
  return seconds * ticks_per_second +
         (remainder_us * ticks_per_second + 999999U) / 1000000U;
}

static void note_overrun(ikb_glue_policy_t *policy) {
  if (policy != NULL) {
    policy->overrun_latched = true;
  }
}

bool ikb_glue_timer_elapsed(ikb_glue_policy_t *policy) {
  if (!ikb_glue_admission_open(policy)) {
    return false;
  }
  if (policy->elapsed_periods < UINT32_MAX) {
    ++policy->elapsed_periods;
  } else {
    note_overrun(policy);
  }
  if (policy->aggregate_pending) {
    note_overrun(policy);
    return false;
  }
  policy->aggregate_pending = true;
  return true;
}

bool ikb_glue_aggregate_begin(ikb_glue_policy_t *policy,
                              uint32_t *elapsed_periods) {
  if (!ikb_glue_admission_open(policy) || elapsed_periods == NULL ||
      !policy->aggregate_pending) {
    return false;
  }
  if (policy->elapsed_periods == 0U) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  *elapsed_periods = policy->elapsed_periods;
  policy->elapsed_periods = 0U;
  policy->aggregate_stop_eligible =
      policy->lifecycle == IKB_GLUE_STOP_REQUESTED;
  return true;
}

bool ikb_glue_aggregate_finish(ikb_glue_policy_t *policy) {
  if (policy == NULL || !policy->aggregate_pending) {
    return false;
  }
  policy->aggregate_pending = false;
  policy->aggregate_stop_eligible = false;
  return true;
}

bool ikb_glue_apply_overrun_flag(ikb_glue_policy_t *policy,
                                 uint32_t *frame_flags) {
  if (policy == NULL || frame_flags == NULL || !policy->overrun_latched) {
    return false;
  }
  *frame_flags |= IKB_FLAG_ACQUISITION_OVERRUN;
  policy->overrun_latched = false;
  return true;
}

static bool within_request_deadline(const ikb_glue_policy_t *policy,
                                    uint64_t now_us, uint64_t limit_us) {
  return now_us >= policy->request_accepted_time_us &&
         now_us - policy->request_accepted_time_us <= limit_us;
}

bool ikb_glue_final_boundary_ready(ikb_glue_policy_t *policy,
                                   uint64_t now_us, bool cache_ready,
                                   bool sensors_idle,
                                   bool ordinary_deadline_reachable) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_STOP_REQUESTED ||
      policy->failure_latched) {
    return false;
  }
  policy->boundary_armed = false;
  if (!within_request_deadline(policy, now_us,
                               IKB_GLUE_BOUNDARY_TIMEOUT_US)) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  if (!policy->aggregate_pending || !policy->aggregate_stop_eligible ||
      policy->overrun_latched || !cache_ready || !sensors_idle ||
      !ordinary_deadline_reachable || policy->final_committed) {
    return false;
  }
  policy->boundary_armed = true;
  return true;
}

bool ikb_glue_close_boundary(ikb_glue_policy_t *policy) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_STOP_REQUESTED ||
      policy->failure_latched || !policy->boundary_armed) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  policy->boundary_armed = false;
  policy->lifecycle = IKB_GLUE_BOUNDARY_CLOSED;
  return true;
}

bool ikb_glue_begin_shutdown(ikb_glue_policy_t *policy) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_BOUNDARY_CLOSED ||
      policy->failure_latched || !policy->final_committed) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  policy->lifecycle = IKB_GLUE_SHUTTING_DOWN;
  return true;
}

bool ikb_glue_begin_marking(ikb_glue_policy_t *policy, uint64_t now_us) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_SHUTTING_DOWN ||
      policy->failure_latched ||
      !within_request_deadline(policy, now_us,
                               IKB_GLUE_RECEIPT_TIMEOUT_US)) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  policy->lifecycle = IKB_GLUE_MARKING;
  return true;
}

bool ikb_glue_marker_fell(ikb_glue_policy_t *policy, uint64_t now_us) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_MARKING ||
      policy->failure_latched ||
      !within_request_deadline(policy, now_us,
                               IKB_GLUE_RECEIPT_TIMEOUT_US)) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  policy->lifecycle = IKB_GLUE_RECEIPT_PENDING;
  return true;
}

static ikb_glue_transport_kind_t active_transport_kind(
    const ikb_glue_policy_t *policy) {
  if (policy == NULL) {
    return IKB_GLUE_TRANSPORT_COUNT;
  }
  for (ikb_glue_transport_kind_t kind = IKB_GLUE_TRANSPORT_NORMAL;
       kind < IKB_GLUE_TRANSPORT_COUNT;
       kind = (ikb_glue_transport_kind_t)(kind + 1)) {
    if (policy->transport[kind].pending) {
      return kind;
    }
  }
  return IKB_GLUE_TRANSPORT_COUNT;
}

static ikb_transport_slot_t *active_transport(ikb_glue_policy_t *policy) {
  const ikb_glue_transport_kind_t kind = active_transport_kind(policy);
  return kind == IKB_GLUE_TRANSPORT_COUNT ? NULL : &policy->transport[kind];
}

bool ikb_glue_transport_offer(ikb_glue_policy_t *policy,
                              const uint8_t *bytes, size_t length) {
  if (policy != NULL && policy->lifecycle <= IKB_GLUE_STOP_REQUESTED &&
      !policy->final_committed &&
      ikb_transport_slot_offer(
          &policy->transport[IKB_GLUE_TRANSPORT_NORMAL], bytes, length)) {
    return true;
  }
  if (policy != NULL && policy->lifecycle >= IKB_GLUE_STOP_REQUESTED) {
    ikb_glue_policy_fail(policy);
  } else {
    note_overrun(policy);
  }
  return false;
}

bool ikb_glue_transport_offer_final(ikb_glue_policy_t *policy,
                                    const uint8_t *bytes, size_t length) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_BOUNDARY_CLOSED ||
      policy->failure_latched || policy->final_committed ||
      !ikb_transport_slot_offer(
          &policy->transport[IKB_GLUE_TRANSPORT_FINAL], bytes, length)) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  policy->final_committed = true;
  return true;
}

bool ikb_glue_transport_offer_receipt(ikb_glue_policy_t *policy,
                                      const uint8_t *bytes, size_t length,
                                      uint64_t now_us) {
  if (policy == NULL || policy->lifecycle != IKB_GLUE_RECEIPT_PENDING ||
      policy->failure_latched || !policy->final_committed ||
      !within_request_deadline(policy, now_us,
                               IKB_GLUE_RECEIPT_TIMEOUT_US) ||
      !ikb_transport_slot_offer(
          &policy->transport[IKB_GLUE_TRANSPORT_RECEIPT], bytes, length)) {
    ikb_glue_policy_fail(policy);
    return false;
  }
  policy->lifecycle = IKB_GLUE_QUIESCED;
  return true;
}

size_t ikb_glue_transport_peek(const ikb_glue_policy_t *policy,
                               const uint8_t **bytes) {
  if (bytes == NULL) {
    return 0U;
  }
  const ikb_glue_transport_kind_t kind = active_transport_kind(policy);
  if (kind == IKB_GLUE_TRANSPORT_COUNT) {
    *bytes = NULL;
    return 0U;
  }
  return ikb_transport_slot_peek(&policy->transport[kind], bytes);
}

ikb_glue_transport_result_t ikb_glue_transport_written(
    ikb_glue_policy_t *policy, int written) {
  ikb_transport_slot_t *slot = active_transport(policy);
  const uint8_t *bytes = NULL;
  const size_t remaining = ikb_transport_slot_peek(slot, &bytes);
  if (slot == NULL || remaining == 0U || written <= 0 ||
      (size_t)written > remaining ||
      !ikb_transport_slot_consume(slot, (size_t)written)) {
    ikb_glue_transport_drop(policy);
    return IKB_GLUE_TRANSPORT_DROPPED;
  }
  return (size_t)written == remaining
             ? IKB_GLUE_TRANSPORT_COMPLETE
             : IKB_GLUE_TRANSPORT_PENDING;
}

void ikb_glue_transport_drop(ikb_glue_policy_t *policy) {
  if (policy == NULL) {
    return;
  }
  ikb_transport_slot_t *slot = active_transport(policy);
  if (slot != NULL) {
    ikb_transport_slot_drop(slot);
  }
  if (policy->lifecycle >= IKB_GLUE_STOP_REQUESTED) {
    ikb_glue_policy_fail(policy);
  } else {
    note_overrun(policy);
  }
}
