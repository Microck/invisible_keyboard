#include "firmware/acquisition.h"

#include <limits.h>
#include <string.h>

static bool sample_offset(uint64_t sample_time_us, uint64_t target_time_us,
                          int16_t *offset_us) {
  if (sample_time_us >= target_time_us) {
    const uint64_t forward = sample_time_us - target_time_us;
    if (forward > (uint64_t)INT16_MAX) {
      return false;
    }
    *offset_us = (int16_t)forward;
    return true;
  }

  const uint64_t backward = target_time_us - sample_time_us;
  if (backward > (uint64_t)INT16_MAX + 1U) {
    return false;
  }
  if (backward == (uint64_t)INT16_MAX + 1U) {
    *offset_us = INT16_MIN;
  } else {
    *offset_us = -(int16_t)backward;
  }
  return true;
}

bool ikb_acquisition_init(
    ikb_acquisition_state_t *state, ikb_hand_t hand,
    const uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN],
    const uint8_t boot_id[IKB_BOOT_ID_LEN]) {
  if (state == NULL ||
      (hand != IKB_HAND_LEFT && hand != IKB_HAND_RIGHT) ||
      controller_id_sha256 == NULL || boot_id == NULL ||
      !ikb_has_nonzero_byte(controller_id_sha256,
                            IKB_CONTROLLER_ID_SHA256_LEN) ||
      !ikb_has_nonzero_byte(boot_id, IKB_BOOT_ID_LEN)) {
    return false;
  }
  state->hand = hand;
  memcpy(state->controller_id_sha256, controller_id_sha256,
         sizeof(state->controller_id_sha256));
  memcpy(state->boot_id, boot_id, sizeof(state->boot_id));
  state->next_sequence = 0U;
  state->last_device_time_us = 0U;
  state->has_device_time = false;
  state->clock_fault_latched = false;
  return true;
}

bool ikb_acquire_frame(ikb_acquisition_state_t *state,
                       const ikb_acquisition_hal_t *hal,
                       uint64_t target_time_us, uint64_t deadline_time_us,
                       ikb_sensor_frame_t *frame) {
  if (state == NULL || hal == NULL || hal->read_latest == NULL ||
      hal->now_us == NULL || frame == NULL ||
      deadline_time_us < target_time_us) {
    return false;
  }

  memset(frame, 0, sizeof(*frame));
  frame->hand = state->hand;
  memcpy(frame->controller_id_sha256, state->controller_id_sha256,
         sizeof(frame->controller_id_sha256));
  memcpy(frame->boot_id, state->boot_id, sizeof(frame->boot_id));
  frame->sequence = state->next_sequence++;
  frame->device_time_us = target_time_us;
  if (state->has_device_time &&
      target_time_us <= state->last_device_time_us) {
    state->clock_fault_latched = true;
  }
  state->last_device_time_us = target_time_us;
  state->has_device_time = true;

  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    ikb_hal_sample_t sample = {0};
    if (!hal->read_latest(hal->context, sensor, &sample)) {
      frame->flags |= IKB_FLAG_ACQUISITION_OVERRUN;
      continue;
    }
    if (sample.acquisition_overrun) {
      frame->flags |= IKB_FLAG_ACQUISITION_OVERRUN;
    }
    if (!sample.fresh) {
      frame->flags |= IKB_FLAG_ACQUISITION_OVERRUN;
      continue;
    }

    int16_t offset_us = 0;
    if (!sample_offset(sample.sample_time_us, target_time_us, &offset_us)) {
      frame->flags |= IKB_FLAG_ACQUISITION_OVERRUN;
      continue;
    }

    frame->sensors[sensor] = sample.reading;
    frame->sensors[sensor].sample_time_offset_us = offset_us;
    frame->valid_sensor_mask |= (uint8_t)(1U << sensor);
  }

  const uint64_t completion_time_us = hal->now_us(hal->context);
  if (completion_time_us < target_time_us) {
    state->clock_fault_latched = true;
  } else if (completion_time_us > deadline_time_us) {
    frame->flags |= IKB_FLAG_ACQUISITION_OVERRUN;
  }
  if (state->clock_fault_latched) {
    frame->flags |= IKB_FLAG_CLOCK_UNSYNCED;
  }
  return true;
}
