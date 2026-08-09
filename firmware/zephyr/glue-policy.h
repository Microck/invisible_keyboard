#ifndef INVISIBLE_KEYBOARD_ZEPHYR_GLUE_POLICY_H_
#define INVISIBLE_KEYBOARD_ZEPHYR_GLUE_POLICY_H_

#include "controller.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define IKB_GLUE_BOUNDARY_TIMEOUT_US 400000U
#define IKB_GLUE_RECEIPT_TIMEOUT_US 500000U

/*
 * These states preserve the admission-to-submission gap. The board adapter
 * must enter ADMITTED_UNSUBMITTED before calling k_work_submit_to_queue and
 * record either SUBMITTED or a permanent policy fault before unlocking.
 */
typedef enum {
  IKB_GLUE_SENSOR_IDLE = 0,
  IKB_GLUE_SENSOR_ADMITTED_UNSUBMITTED = 1,
  IKB_GLUE_SENSOR_SUBMITTED = 2,
  IKB_GLUE_SENSOR_RUNNING = 3,
  IKB_GLUE_SENSOR_CLOSED = 4,
} ikb_glue_sensor_state_t;

typedef struct {
  uint64_t edge_time_us;
  ikb_glue_sensor_state_t state;
} ikb_glue_sensor_policy_t;

void ikb_glue_sensor_initialize(ikb_glue_sensor_policy_t *sensor);
ikb_glue_sensor_state_t ikb_glue_sensor_state(
    const ikb_glue_sensor_policy_t *sensor);
bool ikb_glue_sensor_admit(ikb_glue_sensor_policy_t *sensor,
                           uint64_t edge_time_us);
bool ikb_glue_sensor_mark_submitted(ikb_glue_sensor_policy_t *sensor);
bool ikb_glue_sensor_abort_submission(ikb_glue_sensor_policy_t *sensor);
bool ikb_glue_sensor_begin(ikb_glue_sensor_policy_t *sensor,
                           uint64_t *edge_time_us);
bool ikb_glue_sensor_pending_edge(const ikb_glue_sensor_policy_t *sensor,
                                  uint64_t *edge_time_us);
bool ikb_glue_sensor_complete(ikb_glue_sensor_policy_t *sensor);
bool ikb_glue_sensor_close(ikb_glue_sensor_policy_t *sensor);

typedef enum {
  IKB_GLUE_RUNNING = 0,
  IKB_GLUE_STOP_REQUESTED = 1,
  IKB_GLUE_BOUNDARY_CLOSED = 2,
  IKB_GLUE_SHUTTING_DOWN = 3,
  IKB_GLUE_MARKING = 4,
  IKB_GLUE_RECEIPT_PENDING = 5,
  IKB_GLUE_QUIESCED = 6,
} ikb_glue_lifecycle_t;

typedef enum {
  IKB_GLUE_TRANSPORT_PENDING = 0,
  IKB_GLUE_TRANSPORT_COMPLETE = 1,
  IKB_GLUE_TRANSPORT_DROPPED = 2,
} ikb_glue_transport_result_t;

typedef enum {
  IKB_GLUE_TRANSPORT_NORMAL = 0,
  IKB_GLUE_TRANSPORT_FINAL = 1,
  IKB_GLUE_TRANSPORT_RECEIPT = 2,
  IKB_GLUE_TRANSPORT_COUNT = 3,
} ikb_glue_transport_kind_t;

/*
 * One portable lifecycle shared by GPIO admission, the aggregate timer, the
 * terminal transport, and the Zephyr shutdown adapter. Every board-side call
 * is protected by the outer glue-policy lock. The full board lock order is:
 * glue policy, sample cache, sensor admission state.
 */
typedef struct {
  ikb_transport_slot_t transport[IKB_GLUE_TRANSPORT_COUNT];
  ikb_quiesce_request_t request;
  uint64_t request_accepted_time_us;
  uint32_t elapsed_periods;
  ikb_glue_lifecycle_t lifecycle;
  bool aggregate_pending;
  bool aggregate_stop_eligible;
  bool boundary_armed;
  bool final_committed;
  bool overrun_latched;
  bool failure_latched;
} ikb_glue_policy_t;

void ikb_glue_policy_initialize(ikb_glue_policy_t *policy);
void ikb_glue_policy_fail(ikb_glue_policy_t *policy);
bool ikb_glue_policy_failed(const ikb_glue_policy_t *policy);
ikb_glue_lifecycle_t ikb_glue_policy_lifecycle(
    const ikb_glue_policy_t *policy);
bool ikb_glue_admission_open(const ikb_glue_policy_t *policy);

/*
 * Accept one already decoded request and bind it to this boot identity. Any
 * mismatch, duplicate, or call after a prior failure permanently suppresses
 * Gate 0A success while pre-boundary acquisition may continue.
 */
bool ikb_glue_request_quiesce(
    ikb_glue_policy_t *policy, const ikb_quiesce_request_t *request,
    ikb_hand_t expected_hand,
    const uint8_t expected_controller_id[IKB_CONTROLLER_ID_SHA256_LEN],
    const uint8_t expected_boot_id[IKB_BOOT_ID_LEN], uint64_t accepted_time_us);
bool ikb_glue_quiesce_request(const ikb_glue_policy_t *policy,
                              ikb_quiesce_request_t *request);

/*
 * Advance an uptime-anchored target by exactly 5000 us and return its absolute
 * ceiling tick. The caller supplies a positive tick rate and a target within
 * the kernel's uptime range. Never feed the rounded tick back into the target.
 */
uint64_t ikb_glue_next_aggregate_tick(uint64_t *target_time_us,
                                      uint32_t ticks_per_second);

/* Return true only when the timer must wake the aggregate worker. */
bool ikb_glue_timer_elapsed(ikb_glue_policy_t *policy);

/* Consume all timer periods admitted for this aggregate execution. */
bool ikb_glue_aggregate_begin(ikb_glue_policy_t *policy,
                              uint32_t *elapsed_periods);
bool ikb_glue_aggregate_finish(ikb_glue_policy_t *policy);

bool ikb_glue_apply_overrun_flag(ikb_glue_policy_t *policy,
                                 uint32_t *frame_flags);

/*
 * Arm one scheduled final aggregate only while the cache is complete and
 * clean, every sensor is idle, terminal slots are reserved, and its ordinary
 * deadline remains reachable. The 400 ms request deadline is checked here.
 */
bool ikb_glue_final_boundary_ready(ikb_glue_policy_t *policy,
                                   uint64_t now_us, bool cache_ready,
                                   bool sensors_idle,
                                   bool ordinary_deadline_reachable);
bool ikb_glue_close_boundary(ikb_glue_policy_t *policy);
bool ikb_glue_begin_shutdown(ikb_glue_policy_t *policy);
bool ikb_glue_begin_marking(ikb_glue_policy_t *policy, uint64_t now_us);
bool ikb_glue_marker_fell(ikb_glue_policy_t *policy, uint64_t now_us);

/*
 * The three fixed slots form one ordered queue: normal, final SensorFrame,
 * receipt. Normal traffic can use only its own reusable slot. The final and
 * receipt slots are reserved when the stop request is accepted.
 */
bool ikb_glue_transport_offer(ikb_glue_policy_t *policy,
                              const uint8_t *bytes, size_t length);
bool ikb_glue_transport_offer_final(ikb_glue_policy_t *policy,
                                    const uint8_t *bytes, size_t length);
bool ikb_glue_transport_offer_receipt(ikb_glue_policy_t *policy,
                                      const uint8_t *bytes, size_t length,
                                      uint64_t now_us);
size_t ikb_glue_transport_peek(const ikb_glue_policy_t *policy,
                               const uint8_t **bytes);
ikb_glue_transport_result_t ikb_glue_transport_written(
    ikb_glue_policy_t *policy, int written);
void ikb_glue_transport_drop(ikb_glue_policy_t *policy);

#endif
