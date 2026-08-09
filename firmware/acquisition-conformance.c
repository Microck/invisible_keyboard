#include "firmware/acquisition.h"

#include <limits.h>
#include <stdio.h>
#include <string.h>

typedef struct {
  ikb_hal_sample_t samples[IKB_SENSOR_COUNT];
  uint8_t call_order[IKB_SENSOR_COUNT];
  size_t call_count;
  uint8_t failed_read_mask;
  uint64_t now_us;
} fake_sensor_bus_t;

static bool expect(bool condition, const char *message) {
  if (condition) {
    return true;
  }
  fprintf(stderr, "acquisition conformance failure: %s\n", message);
  return false;
}

static bool read_latest(void *context, uint8_t sensor_index,
                        ikb_hal_sample_t *sample) {
  fake_sensor_bus_t *bus = context;
  if (sensor_index >= IKB_SENSOR_COUNT || sample == NULL ||
      bus->call_count >= IKB_SENSOR_COUNT) {
    return false;
  }
  bus->call_order[bus->call_count++] = sensor_index;
  if ((bus->failed_read_mask & (uint8_t)(1U << sensor_index)) != 0U) {
    return false;
  }
  *sample = bus->samples[sensor_index];
  return true;
}

static uint64_t now_us(void *context) {
  const fake_sensor_bus_t *bus = context;
  return bus->now_us;
}

static void prepare_bus(fake_sensor_bus_t *bus, uint64_t target_time_us) {
  memset(bus, 0, sizeof(*bus));
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    bus->samples[sensor].fresh = true;
    bus->samples[sensor].sample_time_us =
        target_time_us - ((uint64_t)sensor * 100U);
    bus->samples[sensor].reading.acceleration[0] = (int16_t)sensor;
    bus->samples[sensor].reading.angular_velocity[2] = -(int16_t)sensor;
  }
  bus->now_us = target_time_us + 1000U;
}

int main(void) {
  const uint64_t target_time_us = 100000U;
  const uint8_t left_controller_id[IKB_CONTROLLER_ID_SHA256_LEN] = {[0] = 0x11};
  const uint8_t right_controller_id[IKB_CONTROLLER_ID_SHA256_LEN] = {[0] = 0x22};
  const uint8_t left_boot_id[IKB_BOOT_ID_LEN] = {[0] = 0x31};
  const uint8_t right_boot_id[IKB_BOOT_ID_LEN] = {[0] = 0x32};
  fake_sensor_bus_t bus;
  prepare_bus(&bus, target_time_us);

  ikb_acquisition_state_t state;
  ikb_sensor_frame_t frame;
  const ikb_acquisition_hal_t hal = {
      .context = &bus,
      .read_latest = read_latest,
      .now_us = now_us,
  };
  if (!expect(ikb_acquisition_init(&state, IKB_HAND_LEFT, left_controller_id,
                                   left_boot_id),
              "valid hand initializes") ||
      !expect(ikb_acquire_frame(&state, &hal, target_time_us,
                                target_time_us + 5000U, &frame),
              "complete frame acquired") ||
      !expect(frame.hand == IKB_HAND_LEFT && frame.sequence == 0U &&
                  memcmp(frame.controller_id_sha256, left_controller_id,
                         sizeof(left_controller_id)) == 0 &&
                  memcmp(frame.boot_id, left_boot_id,
                         sizeof(left_boot_id)) == 0,
              "identity and first sequence") ||
      !expect(frame.device_time_us == target_time_us,
              "target time becomes aggregate time") ||
      !expect(frame.valid_sensor_mask == IKB_ALL_SENSORS_VALID,
              "all fresh sensors valid") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) == 0U,
              "fresh local clock needs no host control signal") ||
      !expect((frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) == 0U,
              "on-time acquisition is not an overrun") ||
      !expect(frame.sensors[5].sample_time_offset_us == -500,
              "sample time offset retained") ||
      !expect(bus.call_count == IKB_SENSOR_COUNT,
              "every sensor read once")) {
    return 1;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!expect(bus.call_order[sensor] == sensor,
                "canonical sensor read order")) {
      return 1;
    }
  }

  ikb_acquisition_state_t clock_fault_state;
  prepare_bus(&bus, target_time_us);
  if (!expect(ikb_acquisition_init(&clock_fault_state, IKB_HAND_LEFT,
                                   left_controller_id, left_boot_id),
              "clock fault state initializes") ||
      !expect(ikb_acquire_frame(&clock_fault_state, &hal, target_time_us,
                                target_time_us + 5000U, &frame),
              "first local clock reading is valid evidence") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) == 0U,
              "first local clock reading is reliable")) {
    return 1;
  }

  const uint64_t reset_time_us = target_time_us - 10000U;
  prepare_bus(&bus, reset_time_us);
  if (!expect(ikb_acquire_frame(&clock_fault_state, &hal, reset_time_us,
                                reset_time_us + 5000U, &frame),
              "regressed clock remains evidence") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) != 0U,
              "controller clock regression is reported")) {
    return 1;
  }

  prepare_bus(&bus, reset_time_us + 5000U);
  if (!expect(ikb_acquire_frame(&clock_fault_state, &hal,
                                reset_time_us + 5000U,
                                reset_time_us + 10000U, &frame),
              "post-reset clock remains evidence") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) != 0U,
              "detected clock fault stays latched for the boot epoch")) {
    return 1;
  }

  prepare_bus(&bus, target_time_us);
  if (!expect(ikb_acquisition_init(&clock_fault_state, IKB_HAND_LEFT,
                                   left_controller_id, left_boot_id),
              "new controller boot clears the prior clock fault") ||
      !expect(ikb_acquire_frame(&clock_fault_state, &hal, target_time_us,
                                target_time_us + 5000U, &frame),
              "new boot emits local clock evidence") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) == 0U,
              "new boot starts with a reliable local clock")) {
    return 1;
  }

  const uint64_t next_target_time_us = target_time_us + 5000U;
  prepare_bus(&bus, next_target_time_us);
  bus.now_us = next_target_time_us - 1U;
  if (!expect(ikb_acquire_frame(&clock_fault_state, &hal,
                                next_target_time_us,
                                next_target_time_us + 5000U, &frame),
              "clock reset during acquisition remains evidence") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) != 0U,
              "clock reset during acquisition is reported")) {
    return 1;
  }

  ikb_acquisition_state_t fault_state;
  prepare_bus(&bus, target_time_us);
  bus.failed_read_mask = (uint8_t)(1U << 2U);
  if (!expect(ikb_acquisition_init(&fault_state, IKB_HAND_RIGHT,
                                   right_controller_id, right_boot_id),
              "fault accounting state initializes") ||
      !expect(ikb_acquire_frame(&fault_state, &hal, target_time_us,
                                target_time_us + 5000U, &frame),
              "failed read remains an evidence frame") ||
      !expect((frame.valid_sensor_mask & (1U << 2U)) == 0U,
              "failed read clears channel validity") ||
      !expect((frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "failed read marks acquisition overrun")) {
    return 1;
  }

  const uint64_t stale_target_time_us = target_time_us + 5000U;
  prepare_bus(&bus, stale_target_time_us);
  bus.samples[3].fresh = false;
  if (!expect(ikb_acquire_frame(&fault_state, &hal, stale_target_time_us,
                                stale_target_time_us + 5000U, &frame),
              "stale read remains an evidence frame") ||
      !expect((frame.valid_sensor_mask & (1U << 3U)) == 0U,
              "stale read clears channel validity") ||
      !expect((frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "stale read marks acquisition overrun")) {
    return 1;
  }

  const uint64_t fault_target_time_us = target_time_us + 10000U;
  prepare_bus(&bus, fault_target_time_us);
  bus.samples[1].fresh = false;
  bus.samples[3].acquisition_overrun = true;
  bus.samples[4].sample_time_us = fault_target_time_us - 40000U;
  bus.now_us = fault_target_time_us + 6000U;
  if (!expect(ikb_acquire_frame(&state, &hal, fault_target_time_us,
                                fault_target_time_us + 5000U, &frame),
              "faulted aggregate is still evidence") ||
      !expect(frame.sequence == 1U, "sequence advances once per frame") ||
      !expect((frame.valid_sensor_mask & (1U << 1U)) == 0U,
              "stale sensor invalidated") ||
      !expect((frame.valid_sensor_mask & (1U << 4U)) == 0U,
              "out-of-range timestamp invalidated") ||
      !expect((frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "driver, deadline, and timestamp faults set overrun") ||
      !expect((frame.flags & IKB_FLAG_CLOCK_UNSYNCED) == 0U,
              "valid clock clears unsynced flag")) {
    return 1;
  }

  const uint64_t boundary_target_time_us = target_time_us + 20000U;
  prepare_bus(&bus, boundary_target_time_us);
  bus.samples[0].sample_time_us =
      boundary_target_time_us + (uint64_t)INT16_MAX;
  bus.samples[1].sample_time_us =
      boundary_target_time_us - ((uint64_t)INT16_MAX + 1U);
  bus.samples[2].sample_time_us =
      boundary_target_time_us + (uint64_t)INT16_MAX + 1U;
  bus.samples[3].sample_time_us =
      boundary_target_time_us - ((uint64_t)INT16_MAX + 2U);
  bus.failed_read_mask = (uint8_t)(1U << 4U);
  if (!expect(ikb_acquire_frame(&state, &hal, boundary_target_time_us,
                                boundary_target_time_us + 5000U, &frame),
              "offset boundary frame acquired") ||
      !expect(frame.sequence == 2U, "boundary frame advances sequence") ||
      !expect((frame.valid_sensor_mask & (1U << 0U)) != 0U &&
                  frame.sensors[0].sample_time_offset_us == INT16_MAX,
              "maximum positive offset accepted") ||
      !expect((frame.valid_sensor_mask & (1U << 1U)) != 0U &&
                  frame.sensors[1].sample_time_offset_us == INT16_MIN,
              "minimum negative offset accepted") ||
      !expect((frame.valid_sensor_mask & (1U << 2U)) == 0U &&
                  (frame.valid_sensor_mask & (1U << 3U)) == 0U,
              "offsets outside int16 range invalidated") ||
      !expect((frame.valid_sensor_mask & (1U << 4U)) == 0U,
              "failed sensor read invalidated") ||
      !expect((frame.valid_sensor_mask & (1U << 5U)) != 0U,
              "later sensor survives an earlier read failure") ||
      !expect((frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "out-of-range offsets mark the aggregate faulted") ||
      !expect(bus.call_count == IKB_SENSOR_COUNT,
              "read failure does not skip later sensors")) {
    return 1;
  }

  const size_t calls_before_invalid = bus.call_count;
  const uint32_t sequence_before_invalid = state.next_sequence;
  ikb_sensor_frame_t unchanged_frame;
  memset(&unchanged_frame, 0xA5, sizeof(unchanged_frame));
  frame = unchanged_frame;
  if (!expect(!ikb_acquire_frame(NULL, &hal, target_time_us,
                                 target_time_us + 5000U, &frame),
              "null state rejected") ||
      !expect(!ikb_acquire_frame(&state, NULL, target_time_us,
                                 target_time_us + 5000U, &frame),
              "null HAL rejected") ||
      !expect(!ikb_acquire_frame(&state, &hal, target_time_us,
                                 target_time_us + 5000U, NULL),
              "null frame rejected") ||
      !expect(!ikb_acquire_frame(&state, &hal, target_time_us,
                                 target_time_us - 1U, &frame),
              "deadline before target rejected") ||
      !expect(bus.call_count == calls_before_invalid,
              "invalid calls do not touch hardware") ||
      !expect(state.next_sequence == sequence_before_invalid,
              "invalid calls do not advance sequence") ||
      !expect(memcmp(&frame, &unchanged_frame, sizeof(frame)) == 0,
              "invalid calls do not mutate output")) {
    return 1;
  }

  const uint8_t zero_controller_id[IKB_CONTROLLER_ID_SHA256_LEN] = {0};
  const uint8_t zero_boot_id[IKB_BOOT_ID_LEN] = {0};
  if (!expect(!ikb_acquisition_init(&state, IKB_HAND_LEFT, NULL,
                                    left_boot_id),
              "null controller identity rejected") ||
      !expect(!ikb_acquisition_init(&state, IKB_HAND_LEFT,
                                    left_controller_id, NULL),
              "null boot identity rejected") ||
      !expect(!ikb_acquisition_init(&state, IKB_HAND_LEFT,
                                    zero_controller_id, left_boot_id),
              "zero controller identity rejected") ||
      !expect(!ikb_acquisition_init(&state, IKB_HAND_LEFT,
                                    left_controller_id, zero_boot_id),
              "zero boot identity rejected")) {
    return 1;
  }

  return 0;
}
