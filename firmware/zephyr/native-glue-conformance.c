#include "glue-policy.h"

#include <stdio.h>
#include <string.h>

static bool expect(bool condition, const char *message) {
  if (condition) {
    return true;
  }
  fprintf(stderr, "zephyr glue conformance failure: %s\n", message);
  return false;
}

static ikb_quiesce_request_t test_request(void) {
  ikb_quiesce_request_t request = {
      .hand = IKB_HAND_LEFT,
      .request_start_time_ns = 123456789U,
  };
  memset(request.controller_id_sha256, 0x11,
         sizeof(request.controller_id_sha256));
  memset(request.boot_id, 0x22, sizeof(request.boot_id));
  memset(request.session_id, 0x33, sizeof(request.session_id));
  return request;
}

static bool accept_request(ikb_glue_policy_t *policy,
                           const ikb_quiesce_request_t *request,
                           uint64_t accepted_time_us) {
  return ikb_glue_request_quiesce(
      policy, request, IKB_HAND_LEFT, request->controller_id_sha256,
      request->boot_id, accepted_time_us);
}

static bool begin_scheduled_aggregate(ikb_glue_policy_t *policy) {
  uint32_t elapsed_periods = 0U;
  return ikb_glue_timer_elapsed(policy) &&
         ikb_glue_aggregate_begin(policy, &elapsed_periods) &&
         elapsed_periods == 1U;
}

static bool consume_exact(ikb_glue_policy_t *policy,
                          const uint8_t *expected, size_t length,
                          size_t first_write) {
  const uint8_t *bytes = NULL;
  if (ikb_glue_transport_peek(policy, &bytes) != length || bytes == NULL ||
      memcmp(bytes, expected, length) != 0) {
    return false;
  }
  if (first_write < length) {
    if (ikb_glue_transport_written(policy, (int)first_write) !=
            IKB_GLUE_TRANSPORT_PENDING ||
        ikb_glue_transport_peek(policy, &bytes) != length - first_write ||
        memcmp(bytes, expected + first_write, length - first_write) != 0) {
      return false;
    }
    return ikb_glue_transport_written(policy,
                                      (int)(length - first_write)) ==
           IKB_GLUE_TRANSPORT_COMPLETE;
  }
  return ikb_glue_transport_written(policy, (int)length) ==
         IKB_GLUE_TRANSPORT_COMPLETE;
}

typedef struct {
  uint64_t sample_time_us;
  uint64_t completion_time_us;
} cadence_input_t;

static bool cadence_read_sample(void *context, uint8_t sensor_index,
                                 ikb_hal_sample_t *sample) {
  const cadence_input_t *input = context;
  if (sensor_index >= IKB_SENSOR_COUNT) {
    return false;
  }
  memset(sample, 0, sizeof(*sample));
  sample->sample_time_us = input->sample_time_us;
  sample->fresh = true;
  return true;
}

static uint64_t cadence_completion_time(void *context) {
  const cadence_input_t *input = context;
  return input->completion_time_us;
}

static bool cadence_conformance(void) {
  enum { TICKS_PER_SECOND = 32768, CLEAN_PERIODS = 720000 };
  /* Non-integral tick/microsecond phase, beyond 32-bit uptime microseconds. */
  const uint64_t anchor_us = UINT64_C(1234567890123);
  uint64_t timer_target_us = anchor_us;
  uint64_t worker_target_us = anchor_us;
  uint64_t previous_tick =
      (anchor_us * TICKS_PER_SECOND + 999999U) / 1000000U;
  const ikb_quiesce_request_t request = test_request();
  ikb_glue_policy_t policy;
  ikb_glue_policy_initialize(&policy);
  ikb_acquisition_state_t acquisition;
  cadence_input_t input = {0};
  const ikb_acquisition_hal_t hal = {
      .context = &input,
      .read_latest = cadence_read_sample,
      .now_us = cadence_completion_time,
  };
  if (!expect(ikb_acquisition_init(&acquisition, IKB_HAND_LEFT,
                                   request.controller_id_sha256,
                                   request.boot_id),
              "cadence regression initializes real acquisition policy")) {
    return false;
  }

  for (uint32_t period = 1U; period <= CLEAN_PERIODS; ++period) {
    const uint64_t tick = ikb_glue_next_aggregate_tick(
        &timer_target_us, TICKS_PER_SECOND);
    const uint64_t scaled_target = timer_target_us * TICKS_PER_SECOND;
    const uint64_t event_time_us = tick * 1000000U / TICKS_PER_SECOND;
    uint32_t elapsed_periods = 0U;
    if (!expect(timer_target_us ==
                    anchor_us + (uint64_t)period * IKB_AGGREGATE_PERIOD_US &&
                    tick * 1000000U >= scaled_target &&
                    tick * 1000000U - scaled_target < 1000000U &&
                    (tick - previous_tick == 163U ||
                     tick - previous_tick == 164U),
                "one hour at 32768 Hz stays within one tick of exact 200 Hz") ||
        !expect(ikb_glue_timer_elapsed(&policy) &&
                    ikb_glue_aggregate_begin(&policy, &elapsed_periods) &&
                    elapsed_periods == 1U,
                "each clean absolute deadline admits exactly one period")) {
      return false;
    }
    worker_target_us += (uint64_t)elapsed_periods * IKB_AGGREGATE_PERIOD_US;
    input.sample_time_us = event_time_us - 100U;
    input.completion_time_us = event_time_us + 100U;
    ikb_sensor_frame_t frame;
    if (!expect(worker_target_us == timer_target_us &&
                    ikb_acquire_frame(&acquisition, &hal, worker_target_us,
                                      worker_target_us +
                                          IKB_AGGREGATE_PERIOD_US,
                                      &frame) &&
                    !ikb_glue_apply_overrun_flag(&policy, &frame.flags) &&
                    frame.flags == 0U &&
                    frame.valid_sensor_mask == IKB_ALL_SENSORS_VALID &&
                    frame.device_time_us == worker_target_us &&
                    frame.sequence == period - 1U &&
                    ikb_glue_aggregate_finish(&policy),
                "long clean acquisition keeps truthful timestamps and flags")) {
      return false;
    }
    previous_tick = tick;
  }

  if (!expect(accept_request(&policy, &request, worker_target_us),
              "long run accepts a quiesce request")) {
    return false;
  }
  /* A late Zephyr clock announcement replays all three expired deadlines.
   * Callback uptime is the scheduled event, not the late wall-clock time.
   */
  for (uint32_t missed = 0U; missed < 3U; ++missed) {
    (void)ikb_glue_next_aggregate_tick(&timer_target_us, TICKS_PER_SECOND);
    if (!expect(ikb_glue_timer_elapsed(&policy) == (missed == 0U),
                "late announcement wakes once but accounts every deadline")) {
      return false;
    }
  }
  uint32_t elapsed_periods = 0U;
  uint32_t flags = 0U;
  if (!expect(ikb_glue_aggregate_begin(&policy, &elapsed_periods) &&
                  elapsed_periods == 3U,
              "late worker consumes all three periods")) {
    return false;
  }
  worker_target_us += (uint64_t)elapsed_periods * IKB_AGGREGATE_PERIOD_US;
  if (!expect(worker_target_us == timer_target_us &&
                  !ikb_glue_final_boundary_ready(
                      &policy, worker_target_us, true, true, true) &&
                  ikb_glue_apply_overrun_flag(&policy, &flags) &&
                  flags == IKB_FLAG_ACQUISITION_OVERRUN,
              "missed periods retain fault evidence and reject final boundary")) {
    return false;
  }
  input.sample_time_us = worker_target_us;
  input.completion_time_us = worker_target_us + IKB_AGGREGATE_PERIOD_US + 1U;
  ikb_sensor_frame_t late_frame;
  if (!expect(ikb_acquire_frame(&acquisition, &hal, worker_target_us,
                                worker_target_us + IKB_AGGREGATE_PERIOD_US,
                                &late_frame) &&
                  late_frame.flags == IKB_FLAG_ACQUISITION_OVERRUN,
              "completion one microsecond after the deadline remains a fault")) {
    return false;
  }
  (void)ikb_glue_next_aggregate_tick(&timer_target_us, TICKS_PER_SECOND);
  if (!expect(!ikb_glue_timer_elapsed(&policy) &&
                  ikb_glue_aggregate_finish(&policy),
              "timer during execution retains pending-period evidence")) {
    return false;
  }
  (void)ikb_glue_next_aggregate_tick(&timer_target_us, TICKS_PER_SECOND);
  flags = 0U;
  if (!expect(ikb_glue_timer_elapsed(&policy) &&
                  ikb_glue_aggregate_begin(&policy, &elapsed_periods) &&
                  elapsed_periods == 2U &&
                  ikb_glue_apply_overrun_flag(&policy, &flags) &&
                  flags == IKB_FLAG_ACQUISITION_OVERRUN &&
                  ikb_glue_aggregate_finish(&policy),
              "in-flight deadline survives finish until the next wake")) {
    return false;
  }
  worker_target_us += (uint64_t)elapsed_periods * IKB_AGGREGATE_PERIOD_US;
  const uint64_t final_tick = ikb_glue_next_aggregate_tick(
      &timer_target_us, TICKS_PER_SECOND);
  const uint64_t final_time_us = final_tick * 1000000U / TICKS_PER_SECOND;
  flags = 0U;
  if (!expect(ikb_glue_timer_elapsed(&policy) &&
                  ikb_glue_aggregate_begin(&policy, &elapsed_periods) &&
                  elapsed_periods == 1U,
              "cadence recovers without changing its anchor")) {
    return false;
  }
  worker_target_us += (uint64_t)elapsed_periods * IKB_AGGREGATE_PERIOD_US;
  input.sample_time_us = final_time_us - 100U;
  input.completion_time_us = final_time_us + 100U;
  ikb_sensor_frame_t final_frame;
  if (!expect(ikb_acquire_frame(&acquisition, &hal, worker_target_us,
                                worker_target_us + IKB_AGGREGATE_PERIOD_US,
                                &final_frame) &&
                  final_frame.flags == 0U &&
                  final_frame.valid_sensor_mask == IKB_ALL_SENSORS_VALID,
              "recovered final acquisition meets the unchanged deadline")) {
    return false;
  }
  return expect(worker_target_us == timer_target_us &&
                    !ikb_glue_apply_overrun_flag(&policy, &flags) &&
                    flags == 0U &&
                    ikb_glue_final_boundary_ready(
                        &policy, final_time_us, true, true,
                        final_time_us <=
                            worker_target_us + IKB_AGGREGATE_PERIOD_US) &&
                    ikb_glue_close_boundary(&policy) &&
                    !ikb_glue_timer_elapsed(&policy) &&
                    policy.elapsed_periods == 0U &&
                    !ikb_glue_policy_failed(&policy),
                "long-run final boundary is reachable and closes timer admission");
}

int main(void) {
  if (!cadence_conformance()) {
    return 1;
  }
  ikb_glue_sensor_policy_t sensor;
  ikb_glue_sensor_initialize(&sensor);
  uint64_t edge_time_us = 0U;
  if (!expect(ikb_glue_sensor_state(&sensor) == IKB_GLUE_SENSOR_IDLE,
              "sensor starts idle") ||
      !expect(ikb_glue_sensor_admit(&sensor, 100U) &&
                  ikb_glue_sensor_state(&sensor) ==
                      IKB_GLUE_SENSOR_ADMITTED_UNSUBMITTED,
              "DRDY linearizes before work submission") ||
      !expect(!ikb_glue_sensor_admit(&sensor, 101U),
              "admitted-unsubmitted rejects an overlapping edge") ||
      !expect(ikb_glue_sensor_pending_edge(&sensor, &edge_time_us) &&
                  edge_time_us == 100U,
              "submission gap retains the exact edge time") ||
      !expect(ikb_glue_sensor_mark_submitted(&sensor) &&
                  ikb_glue_sensor_state(&sensor) ==
                      IKB_GLUE_SENSOR_SUBMITTED,
              "successful queue submission is explicit") ||
      !expect(ikb_glue_sensor_begin(&sensor, &edge_time_us) &&
                  edge_time_us == 100U &&
                  ikb_glue_sensor_state(&sensor) ==
                      IKB_GLUE_SENSOR_RUNNING,
              "worker start preserves the admitted timestamp") ||
      !expect(ikb_glue_sensor_complete(&sensor) &&
                  ikb_glue_sensor_state(&sensor) == IKB_GLUE_SENSOR_IDLE,
              "D9 completion returns the sensor to idle") ||
      !expect(ikb_glue_sensor_close(&sensor) &&
                  ikb_glue_sensor_state(&sensor) == IKB_GLUE_SENSOR_CLOSED,
              "final D8 closes idle admission") ||
      !expect(!ikb_glue_sensor_admit(&sensor, 102U) &&
                  !ikb_glue_sensor_complete(&sensor),
              "closed admission never reopens")) {
    return 1;
  }

  ikb_glue_sensor_initialize(&sensor);
  if (!expect(ikb_glue_sensor_admit(&sensor, 200U) &&
                  ikb_glue_sensor_abort_submission(&sensor) &&
                  ikb_glue_sensor_state(&sensor) == IKB_GLUE_SENSOR_IDLE,
              "failed work submission releases state for ordinary recovery") ||
      !expect(!ikb_glue_sensor_abort_submission(&sensor),
              "only admitted-unsubmitted work can abort")) {
    return 1;
  }

  ikb_glue_policy_t impossible_aggregate_policy;
  ikb_glue_policy_initialize(&impossible_aggregate_policy);
  impossible_aggregate_policy.aggregate_pending = true;
  uint32_t impossible_periods = 7U;
  if (!expect(!ikb_glue_aggregate_begin(&impossible_aggregate_policy,
                                        &impossible_periods) &&
                  ikb_glue_policy_failed(&impossible_aggregate_policy) &&
                  impossible_periods == 7U,
              "zero-period aggregate is rejected instead of invented")) {
    return 1;
  }

  const ikb_quiesce_request_t request = test_request();
  ikb_glue_policy_t split_policy;
  ikb_glue_policy_initialize(&split_policy);
  uint32_t periods = 0U;
  if (!expect(ikb_glue_timer_elapsed(&split_policy) &&
                  ikb_glue_aggregate_begin(&split_policy, &periods),
              "ordinary aggregate begins before the request") ||
      !expect(accept_request(&split_policy, &request, 1000U),
              "one identity-bound request is accepted") ||
      !expect(!ikb_glue_final_boundary_ready(&split_policy, 1000U, true,
                                             true, true),
              "request after aggregate begin waits for the next schedule") ||
      !expect(ikb_glue_aggregate_finish(&split_policy) &&
                  begin_scheduled_aggregate(&split_policy),
              "the next scheduled aggregate is request-eligible") ||
      !expect(!ikb_glue_final_boundary_ready(&split_policy, 1001U, false,
                                             true, true) &&
                  !ikb_glue_final_boundary_ready(&split_policy, 1001U, true,
                                                 false, true) &&
                  !ikb_glue_final_boundary_ready(&split_policy, 1001U, true,
                                                 true, false),
              "cache, sensor, and ordinary deadline gates are independent") ||
      !expect(ikb_glue_final_boundary_ready(&split_policy, 1001U, true,
                                            true, true) &&
                  ikb_glue_close_boundary(&split_policy) &&
                  ikb_glue_policy_lifecycle(&split_policy) ==
                      IKB_GLUE_BOUNDARY_CLOSED &&
                  !ikb_glue_admission_open(&split_policy),
              "one clean scheduled aggregate closes admission")) {
    return 1;
  }

  ikb_glue_policy_t timeout_policy;
  ikb_glue_policy_initialize(&timeout_policy);
  if (!expect(accept_request(&timeout_policy, &request, 10U) &&
                  begin_scheduled_aggregate(&timeout_policy),
              "timeout fixture reaches an eligible aggregate") ||
      !expect(ikb_glue_final_boundary_ready(
                  &timeout_policy, 10U + IKB_GLUE_BOUNDARY_TIMEOUT_US, true,
                  true, true),
              "400 ms boundary is inclusive")) {
    return 1;
  }

  ikb_glue_policy_t overrun_boundary_policy;
  ikb_glue_policy_initialize(&overrun_boundary_policy);
  uint32_t overrun_flags = 0U;
  if (!expect(accept_request(&overrun_boundary_policy, &request, 10U) &&
                  begin_scheduled_aggregate(&overrun_boundary_policy) &&
                  !ikb_glue_timer_elapsed(&overrun_boundary_policy),
              "pending final aggregate records a missed timer period") ||
      !expect(!ikb_glue_final_boundary_ready(
                  &overrun_boundary_policy, 11U, true, true, true),
              "known aggregate overrun cannot close the final D8 boundary") ||
      !expect(ikb_glue_apply_overrun_flag(&overrun_boundary_policy,
                                          &overrun_flags) &&
                  (overrun_flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "deferred ordinary frame retains the rejected overrun evidence")) {
    return 1;
  }

  ikb_glue_policy_initialize(&timeout_policy);
  if (!expect(accept_request(&timeout_policy, &request, 10U) &&
                  begin_scheduled_aggregate(&timeout_policy),
              "late timeout fixture reaches an eligible aggregate") ||
      !expect(!ikb_glue_final_boundary_ready(
                  &timeout_policy, 11U + IKB_GLUE_BOUNDARY_TIMEOUT_US, true,
                  true, true) &&
                  ikb_glue_policy_failed(&timeout_policy) &&
                  ikb_glue_admission_open(&timeout_policy),
              "401 ms suppresses success without discarding pre-cut work")) {
    return 1;
  }

  ikb_glue_policy_t duplicate_policy;
  ikb_glue_policy_initialize(&duplicate_policy);
  if (!expect(accept_request(&duplicate_policy, &request, 100U),
              "duplicate fixture accepts its first request") ||
      !expect(!accept_request(&duplicate_policy, &request, 101U) &&
                  ikb_glue_policy_failed(&duplicate_policy),
              "a second candidate permanently suppresses success")) {
    return 1;
  }

  ikb_quiesce_request_t wrong_identity = request;
  wrong_identity.controller_id_sha256[0] ^= 0xffU;
  ikb_glue_policy_t wrong_policy;
  ikb_glue_policy_initialize(&wrong_policy);
  if (!expect(!ikb_glue_request_quiesce(
                  &wrong_policy, &wrong_identity, IKB_HAND_LEFT,
                  request.controller_id_sha256, request.boot_id, 100U) &&
                  ikb_glue_policy_failed(&wrong_policy),
              "wrong controller identity fails closed")) {
    return 1;
  }

  const uint8_t normal[] = {0x10U, 0x11U, 0x12U};
  const uint8_t final[] = {0x20U, 0x21U, 0x22U, 0x23U};
  const uint8_t receipt[] = {0x30U, 0x31U};
  ikb_glue_policy_t ordered_policy;
  ikb_glue_policy_initialize(&ordered_policy);
  if (!expect(ikb_glue_transport_offer(&ordered_policy, normal,
                                       sizeof(normal)),
              "running transport admits one normal record") ||
      !expect(accept_request(&ordered_policy, &request, 500U) &&
                  begin_scheduled_aggregate(&ordered_policy) &&
                  ikb_glue_final_boundary_ready(&ordered_policy, 501U, true,
                                                true, true) &&
                  ikb_glue_close_boundary(&ordered_policy),
              "terminal capacity is reserved before boundary close") ||
      !expect(ikb_glue_transport_offer_final(&ordered_policy, final,
                                             sizeof(final)) &&
                  ikb_glue_aggregate_finish(&ordered_policy) &&
                  ikb_glue_begin_shutdown(&ordered_policy),
              "final frame commits immutably before shutdown") ||
      !expect(ikb_glue_begin_marking(&ordered_policy, 502U) &&
                  ikb_glue_marker_fell(&ordered_policy, 503U) &&
                  ikb_glue_transport_offer_receipt(
                      &ordered_policy, receipt, sizeof(receipt), 504U) &&
                  ikb_glue_policy_lifecycle(&ordered_policy) ==
                      IKB_GLUE_QUIESCED,
              "receipt commits only after marker fall") ||
      !expect(consume_exact(&ordered_policy, normal, sizeof(normal), 1U) &&
                  consume_exact(&ordered_policy, final, sizeof(final), 2U) &&
                  consume_exact(&ordered_policy, receipt, sizeof(receipt),
                                sizeof(receipt)),
              "normal, final, and receipt preserve order across partial writes")) {
    return 1;
  }
  const uint8_t *pending = NULL;
  if (!expect(ikb_glue_transport_peek(&ordered_policy, &pending) == 0U &&
                  pending == NULL,
              "receipt completion leaves terminal transport silent") ||
      !expect(!ikb_glue_transport_offer(&ordered_policy, normal,
                                        sizeof(normal)) &&
                  ikb_glue_policy_failed(&ordered_policy),
              "no application byte can follow the receipt")) {
    return 1;
  }

  ikb_glue_policy_t receipt_revocation_policy;
  ikb_glue_policy_initialize(&receipt_revocation_policy);
  if (!expect(accept_request(&receipt_revocation_policy, &request, 600U) &&
                  begin_scheduled_aggregate(&receipt_revocation_policy) &&
                  ikb_glue_final_boundary_ready(
                      &receipt_revocation_policy, 601U, true, true, true) &&
                  ikb_glue_close_boundary(&receipt_revocation_policy) &&
                  ikb_glue_transport_offer_final(
                      &receipt_revocation_policy, final, sizeof(final)) &&
                  ikb_glue_aggregate_finish(&receipt_revocation_policy) &&
                  ikb_glue_begin_shutdown(&receipt_revocation_policy) &&
                  ikb_glue_begin_marking(&receipt_revocation_policy, 602U) &&
                  ikb_glue_marker_fell(&receipt_revocation_policy, 603U) &&
                  ikb_glue_transport_offer_receipt(
                      &receipt_revocation_policy, receipt, sizeof(receipt),
                      604U),
              "receipt revocation fixture reaches a committed receipt")) {
    return 1;
  }
  ikb_glue_policy_fail(&receipt_revocation_policy);
  if (!expect(ikb_glue_policy_failed(&receipt_revocation_policy) &&
                  consume_exact(&receipt_revocation_policy, final,
                                sizeof(final), sizeof(final)) &&
                  ikb_glue_transport_peek(&receipt_revocation_policy,
                                          &pending) == 0U,
              "a late terminal fault drops every unsent receipt byte")) {
    return 1;
  }

  ikb_glue_policy_t backpressure_policy;
  ikb_glue_policy_initialize(&backpressure_policy);
  if (!expect(ikb_glue_transport_offer(&backpressure_policy, normal,
                                       sizeof(normal)) &&
                  !ikb_glue_transport_offer(&backpressure_policy, normal,
                                            sizeof(normal)),
              "running overlap is bounded") ||
      !expect(!ikb_glue_policy_failed(&backpressure_policy),
              "ordinary backpressure remains frame fault evidence") ||
      !expect(ikb_glue_transport_written(&backpressure_policy, 0) ==
                      IKB_GLUE_TRANSPORT_DROPPED &&
                  !ikb_glue_policy_failed(&backpressure_policy),
              "ordinary driver failure does not invent quiesce success")) {
    return 1;
  }

  uint8_t full_frame_record[IKB_TRANSPORT_MAX_LEN];
  uint8_t full_receipt_record[IKB_QUIESCE_RECEIPT_TRANSPORT_LEN];
  memset(full_frame_record, 0x44, sizeof(full_frame_record));
  memset(full_receipt_record, 0x55, sizeof(full_receipt_record));
  for (size_t split = 1U; split < sizeof(full_frame_record); ++split) {
    ikb_glue_policy_t normal_split_policy;
    ikb_glue_policy_initialize(&normal_split_policy);
    if (!expect(ikb_glue_transport_offer(
                    &normal_split_policy, full_frame_record,
                    sizeof(full_frame_record)) &&
                    consume_exact(&normal_split_policy, full_frame_record,
                                  sizeof(full_frame_record), split),
                "every normal-record FIFO split preserves its suffix")) {
      return 1;
    }

    ikb_glue_policy_t final_split_policy;
    ikb_glue_policy_initialize(&final_split_policy);
    if (!expect(accept_request(&final_split_policy, &request, 700U) &&
                    begin_scheduled_aggregate(&final_split_policy) &&
                    ikb_glue_final_boundary_ready(
                        &final_split_policy, 701U, true, true, true) &&
                    ikb_glue_close_boundary(&final_split_policy) &&
                    ikb_glue_transport_offer_final(
                        &final_split_policy, full_frame_record,
                        sizeof(full_frame_record)) &&
                    consume_exact(&final_split_policy, full_frame_record,
                                  sizeof(full_frame_record), split),
                "every final-record FIFO split preserves its suffix")) {
      return 1;
    }
  }

  for (size_t split = 1U; split < sizeof(full_receipt_record); ++split) {
    ikb_glue_policy_t receipt_split_policy;
    ikb_glue_policy_initialize(&receipt_split_policy);
    if (!expect(accept_request(&receipt_split_policy, &request, 800U) &&
                    begin_scheduled_aggregate(&receipt_split_policy) &&
                    ikb_glue_final_boundary_ready(
                        &receipt_split_policy, 801U, true, true, true) &&
                    ikb_glue_close_boundary(&receipt_split_policy) &&
                    ikb_glue_transport_offer_final(
                        &receipt_split_policy, final, sizeof(final)) &&
                    consume_exact(&receipt_split_policy, final,
                                  sizeof(final), sizeof(final)) &&
                    ikb_glue_aggregate_finish(&receipt_split_policy) &&
                    ikb_glue_begin_shutdown(&receipt_split_policy) &&
                    ikb_glue_begin_marking(&receipt_split_policy, 802U) &&
                    ikb_glue_marker_fell(&receipt_split_policy, 803U) &&
                    ikb_glue_transport_offer_receipt(
                        &receipt_split_policy, full_receipt_record,
                        sizeof(full_receipt_record), 804U) &&
                    consume_exact(&receipt_split_policy,
                                  full_receipt_record,
                                  sizeof(full_receipt_record), split),
                "every receipt FIFO split preserves its suffix")) {
      return 1;
    }
  }

  ikb_glue_policy_t receipt_timeout_policy;
  ikb_glue_policy_initialize(&receipt_timeout_policy);
  if (!expect(accept_request(&receipt_timeout_policy, &request, 1000U) &&
                  begin_scheduled_aggregate(&receipt_timeout_policy) &&
                  ikb_glue_final_boundary_ready(&receipt_timeout_policy,
                                                1001U, true, true, true) &&
                  ikb_glue_close_boundary(&receipt_timeout_policy) &&
                  ikb_glue_transport_offer_final(
                      &receipt_timeout_policy, final, sizeof(final)) &&
                  ikb_glue_aggregate_finish(&receipt_timeout_policy) &&
                  ikb_glue_begin_shutdown(&receipt_timeout_policy),
              "receipt timeout fixture reaches shutdown") ||
      !expect(!ikb_glue_begin_marking(
                  &receipt_timeout_policy,
                  1001U + IKB_GLUE_RECEIPT_TIMEOUT_US) &&
                  ikb_glue_policy_failed(&receipt_timeout_policy),
              "receipt offer after 500 ms is suppressed")) {
    return 1;
  }

  return 0;
}
