#ifndef INVISIBLE_KEYBOARD_ACQUISITION_H_
#define INVISIBLE_KEYBOARD_ACQUISITION_H_

#include "firmware/frame-protocol.h"

#include <stdbool.h>
#include <stdint.h>

/* One latest-sample result supplied by the board-specific sensor driver. */
typedef struct {
  ikb_sensor_reading_t reading;
  /* Controller time of the board adapter's declared data-ready edge. */
  uint64_t sample_time_us;
  bool fresh;
  bool acquisition_overrun;
} ikb_hal_sample_t;

typedef bool (*ikb_read_latest_fn)(void *context, uint8_t sensor_index,
                                   ikb_hal_sample_t *sample);
typedef uint64_t (*ikb_now_us_fn)(void *context);

/* Narrow boundary implemented by the selected MCU and sensor driver. */
typedef struct {
  void *context;
  ikb_read_latest_fn read_latest;
  ikb_now_us_fn now_us;
} ikb_acquisition_hal_t;

typedef struct {
  ikb_hand_t hand;
  uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN];
  uint8_t boot_id[IKB_BOOT_ID_LEN];
  uint32_t next_sequence;
  uint64_t last_device_time_us;
  bool has_device_time;
  /* A repeated or regressed local clock invalidates this boot epoch. */
  bool clock_fault_latched;
} ikb_acquisition_state_t;

/* Initialize one per-hand acquisition epoch at controller boot. */
bool ikb_acquisition_init(
    ikb_acquisition_state_t *state, ikb_hand_t hand,
    const uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN],
    const uint8_t boot_id[IKB_BOOT_ID_LEN]);

/*
 * Acquire one six-channel aggregate at a controller-time target.
 *
 * target_time_us and every sample time are local monotonic clock evidence.
 * No host-to-controller synchronization signal is required. A repeated or
 * regressed target, or a controller clock read below the target, latches
 * IKB_FLAG_CLOCK_UNSYNCED until the next controller-boot initialization.
 *
 * A hardware read failure, stale channel, acquisition fault, or out-of-range
 * timestamp remains a successfully emitted evidence frame. The channel-valid
 * mask and aggregate fault bits retain the failure. False is reserved for an
 * invalid programming interface.
 */
bool ikb_acquire_frame(ikb_acquisition_state_t *state,
                       const ikb_acquisition_hal_t *hal,
                       uint64_t target_time_us, uint64_t deadline_time_us,
                       ikb_sensor_frame_t *frame);

#endif
