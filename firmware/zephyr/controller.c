#include "controller.h"

#include <string.h>

enum {
  LSM6DS3TR_C_DRDY_PULSE_CFG_G = 0x0b,
  LSM6DS3TR_C_WHO_AM_I_REG = 0x0f,
  LSM6DS3TR_C_CTRL1_XL = 0x10,
  LSM6DS3TR_C_CTRL2_G = 0x11,
  LSM6DS3TR_C_CTRL3_C = 0x12,
  LSM6DS3TR_C_INT1_CTRL = 0x0d,
  LSM6DS3TR_C_STATUS_REG = 0x1e,
  LSM6DS3TR_C_OUTX_L_G = 0x22,
  LSM6DS3TR_C_SW_RESET = 0x01,
  LSM6DS3TR_C_BDU_IF_INC = 0x44,
  LSM6DS3TR_C_208HZ_4G = 0x58,
  LSM6DS3TR_C_208HZ_1000DPS = 0x58,
  LSM6DS3TR_C_DRDY_PULSED = 0x80,
  LSM6DS3TR_C_INT1_XL_DRDY = 0x01,
  LSM6DS3TR_C_GDA_XLDA = 0x03,
  LSM6DS3TR_C_RESET_POLL_LIMIT = 100,
  LSM6DS3TR_C_RESET_POLL_DELAY_US = 100,
  LSM6DS3TR_C_DATA_POLL_LIMIT = 20,
  LSM6DS3TR_C_DATA_POLL_DELAY_US = 10,
};

static bool bus_is_valid(const ikb_sensor_bus_t *bus) {
  return bus != NULL && bus->select_channel != NULL &&
         bus->write_register != NULL && bus->read_register != NULL &&
         bus->read_registers != NULL && bus->delay_us != NULL;
}

static bool shutdown_bus_is_valid(const ikb_sensor_bus_t *bus) {
  return bus_is_valid(bus) && bus->deselect_all != NULL;
}

static bool write_and_readback(const ikb_sensor_bus_t *bus,
                               uint8_t register_address, uint8_t value) {
  uint8_t readback = 0U;
  return bus->write_register(bus->context, register_address, value) &&
         bus->read_register(bus->context, register_address, &readback) &&
         readback == value;
}

static bool initialize_sensor(const ikb_sensor_bus_t *bus,
                              uint8_t sensor_index) {
  uint8_t value = 0U;
  if (!bus->select_channel(bus->context, sensor_index) ||
      !bus->write_register(bus->context, LSM6DS3TR_C_CTRL3_C,
                           LSM6DS3TR_C_SW_RESET)) {
    return false;
  }

  bool reset_complete = false;
  for (unsigned int poll = 0U; poll < LSM6DS3TR_C_RESET_POLL_LIMIT; ++poll) {
    if (!bus->read_register(bus->context, LSM6DS3TR_C_CTRL3_C, &value)) {
      return false;
    }
    if ((value & LSM6DS3TR_C_SW_RESET) == 0U) {
      reset_complete = true;
      break;
    }
    bus->delay_us(bus->context, LSM6DS3TR_C_RESET_POLL_DELAY_US);
  }
  if (!reset_complete) {
    return false;
  }
  if (!bus->read_register(bus->context, LSM6DS3TR_C_WHO_AM_I_REG, &value) ||
      value != IKB_LSM6DS3TR_C_WHO_AM_I) {
    return false;
  }

  /*
   * BDU makes each 16-bit axis coherent and IF_INC permits one 12-byte burst.
   * Read back every observable profile register so a miswired mux or sensor
   * revision cannot silently produce data with the wrong physical scale.
   */
  return write_and_readback(bus, LSM6DS3TR_C_CTRL3_C,
                            LSM6DS3TR_C_BDU_IF_INC) &&
         write_and_readback(bus, LSM6DS3TR_C_CTRL1_XL,
                            LSM6DS3TR_C_208HZ_4G) &&
         write_and_readback(bus, LSM6DS3TR_C_CTRL2_G,
                            LSM6DS3TR_C_208HZ_1000DPS) &&
         write_and_readback(bus, LSM6DS3TR_C_DRDY_PULSE_CFG_G,
                            LSM6DS3TR_C_DRDY_PULSED) &&
         write_and_readback(bus, LSM6DS3TR_C_INT1_CTRL,
                            0U);
}

bool ikb_sensor_bank_initialize(const ikb_sensor_bus_t *bus) {
  if (!bus_is_valid(bus)) {
    return false;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!initialize_sensor(bus, sensor)) {
      return false;
    }
  }
  return true;
}

bool ikb_sensor_bank_arm(const ikb_sensor_bus_t *bus) {
  if (!bus_is_valid(bus)) {
    return false;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!bus->select_channel(bus->context, sensor) ||
        !write_and_readback(bus, LSM6DS3TR_C_INT1_CTRL,
                            LSM6DS3TR_C_INT1_XL_DRDY)) {
      return false;
    }
  }
  return true;
}

static int16_t get_i16_le(const uint8_t *bytes, size_t offset) {
  return (int16_t)((uint16_t)bytes[offset] |
                   ((uint16_t)bytes[offset + 1U] << 8U));
}

bool ikb_sensor_bank_read(const ikb_sensor_bus_t *bus, uint8_t sensor_index,
                          uint64_t sample_time_us,
                          ikb_hal_sample_t *sample) {
  if (!bus_is_valid(bus) || sensor_index >= IKB_SENSOR_COUNT ||
      sample == NULL) {
    return false;
  }

  memset(sample, 0, sizeof(*sample));
  if (!bus->select_channel(bus->context, sensor_index)) {
    return false;
  }

  sample->sample_time_us = sample_time_us;

  /*
   * INT1 emits one pulse per accelerometer conversion. Both engines use the
   * same ODR, but their register writes are not simultaneous. Poll GDA at most
   * 20 times, inserting at most 190 us of retry delay, so the emitted burst
   * never reuses an old gyro value. A timeout still drains the output block so
   * the next conversion starts from a known register state.
   */
  bool both_ready = false;
  for (unsigned int poll = 0U; poll < LSM6DS3TR_C_DATA_POLL_LIMIT; ++poll) {
    uint8_t status = 0U;
    if (!bus->read_register(bus->context, LSM6DS3TR_C_STATUS_REG, &status)) {
      return false;
    }
    if ((status & LSM6DS3TR_C_GDA_XLDA) == LSM6DS3TR_C_GDA_XLDA) {
      both_ready = true;
      break;
    }
    if (poll + 1U < LSM6DS3TR_C_DATA_POLL_LIMIT) {
      bus->delay_us(bus->context, LSM6DS3TR_C_DATA_POLL_DELAY_US);
    }
  }

  uint8_t output[12] = {0};
  if (!bus->read_registers(bus->context, LSM6DS3TR_C_OUTX_L_G, output,
                           sizeof(output))) {
    return false;
  }
  if (!both_ready) {
    sample->acquisition_overrun = true;
    return true;
  }

  for (size_t axis = 0U; axis < 3U; ++axis) {
    sample->reading.angular_velocity[axis] = get_i16_le(output, axis * 2U);
    sample->reading.acceleration[axis] =
        get_i16_le(output, 6U + axis * 2U);
  }
  sample->fresh = true;
  return true;
}

bool ikb_sensor_bank_shutdown(const ikb_sensor_bus_t *bus) {
  if (!shutdown_bus_is_valid(bus) || !bus->deselect_all(bus->context)) {
    return false;
  }

  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!bus->select_channel(bus->context, sensor) ||
        !write_and_readback(bus, LSM6DS3TR_C_INT1_CTRL, 0U) ||
        !write_and_readback(bus, LSM6DS3TR_C_CTRL1_XL, 0U) ||
        !write_and_readback(bus, LSM6DS3TR_C_CTRL2_G, 0U)) {
      return false;
    }
  }
  return bus->deselect_all(bus->context);
}

void ikb_sample_cache_initialize(ikb_sample_cache_t *cache) {
  if (cache != NULL) {
    memset(cache, 0, sizeof(*cache));
  }
}

void ikb_sample_cache_note_acquisition_fault(ikb_sample_cache_t *cache,
                                             uint8_t sensor_index) {
  if (cache != NULL && sensor_index < IKB_SENSOR_COUNT) {
    cache->acquisition_overrun_latched[sensor_index] = true;
  }
}

static void sample_cache_store_unchecked(ikb_sample_cache_t *cache,
                                         uint8_t sensor_index,
                                         const ikb_hal_sample_t *sample) {
  /* Preserve a real read fault even if a later conversion replaces it. */
  cache->acquisition_overrun_latched[sensor_index] |=
      sample->acquisition_overrun;
  cache->latest[sensor_index] = *sample;
  cache->pending[sensor_index] = true;
}

static bool analyzer_marker_is_valid(const ikb_analyzer_marker_t *marker) {
  return marker != NULL && marker->set != NULL && marker->delay_us != NULL;
}

static bool analyzer_marker_finish(const ikb_analyzer_marker_t *marker,
                                   uint32_t minimum_high_us) {
  marker->delay_us(marker->context, minimum_high_us);
  return marker->set(marker->context, false);
}

bool ikb_analyzer_emit_startup_anchor(
    const ikb_analyzer_marker_t *snapshot_marker,
    const ikb_analyzer_marker_t *commit_marker) {
  if (!analyzer_marker_is_valid(snapshot_marker) ||
      !analyzer_marker_is_valid(commit_marker)) {
    return false;
  }

  if (!snapshot_marker->set(snapshot_marker->context, true) ||
      !analyzer_marker_finish(snapshot_marker,
                              IKB_ANALYZER_STARTUP_PHASE_US)) {
    return false;
  }
  snapshot_marker->delay_us(snapshot_marker->context,
                            IKB_ANALYZER_STARTUP_PHASE_US);
  if (!commit_marker->set(commit_marker->context, true) ||
      !analyzer_marker_finish(commit_marker,
                              IKB_ANALYZER_STARTUP_PHASE_US)) {
    return false;
  }
  commit_marker->delay_us(commit_marker->context,
                          IKB_ANALYZER_STARTUP_PHASE_US);
  return true;
}

bool ikb_sample_cache_commit(
    ikb_sample_cache_t *cache, uint8_t sensor_index,
    const ikb_hal_sample_t *sample,
    const ikb_analyzer_marker_t *commit_marker,
    ikb_sample_ready_fn publish_ready, void *ready_context) {
  if (cache == NULL || sensor_index >= IKB_SENSOR_COUNT || sample == NULL ||
      !analyzer_marker_is_valid(commit_marker) || publish_ready == NULL) {
    return false;
  }
  if (!commit_marker->set(commit_marker->context, true)) {
    publish_ready(ready_context);
    return false;
  }

  sample_cache_store_unchecked(cache, sensor_index, sample);
  const bool marker_cleared = analyzer_marker_finish(
      commit_marker, IKB_ANALYZER_MARKER_MIN_HIGH_US);
  publish_ready(ready_context);
  return marker_cleared;
}

bool ikb_sample_cache_snapshot(
    ikb_sample_cache_t *cache,
    ikb_hal_sample_t samples[IKB_SENSOR_COUNT],
    const ikb_analyzer_marker_t *snapshot_marker) {
  if (cache == NULL || samples == NULL ||
      !analyzer_marker_is_valid(snapshot_marker)) {
    return false;
  }
  if (!snapshot_marker->set(snapshot_marker->context, true)) {
    (void)snapshot_marker->set(snapshot_marker->context, false);
    return false;
  }

  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (cache->pending[sensor]) {
      samples[sensor] = cache->latest[sensor];
      cache->pending[sensor] = false;
    } else {
      memset(&samples[sensor], 0, sizeof(samples[sensor]));
    }
    samples[sensor].acquisition_overrun |=
        cache->acquisition_overrun_latched[sensor];
    cache->acquisition_overrun_latched[sensor] = false;
  }
  return analyzer_marker_finish(snapshot_marker,
                                IKB_ANALYZER_MARKER_MIN_HIGH_US);
}

bool ikb_sample_cache_terminal_ready(const ikb_sample_cache_t *cache) {
  if (cache == NULL) {
    return false;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!cache->pending[sensor] ||
        cache->acquisition_overrun_latched[sensor] ||
        !cache->latest[sensor].fresh ||
        cache->latest[sensor].acquisition_overrun) {
      return false;
    }
  }
  return true;
}

bool ikb_sample_cache_acquisition_faulted(const ikb_sample_cache_t *cache) {
  if (cache == NULL) {
    return true;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (cache->acquisition_overrun_latched[sensor] ||
        (cache->pending[sensor] &&
         (!cache->latest[sensor].fresh ||
          cache->latest[sensor].acquisition_overrun))) {
      return true;
    }
  }
  return false;
}

bool ikb_sample_cache_terminal_snapshot(
    ikb_sample_cache_t *cache,
    ikb_hal_sample_t samples[IKB_SENSOR_COUNT],
    const ikb_analyzer_marker_t *snapshot_marker,
    ikb_close_admission_fn close_admission, void *close_context) {
  if (!ikb_sample_cache_terminal_ready(cache) || samples == NULL ||
      !analyzer_marker_is_valid(snapshot_marker) || close_admission == NULL) {
    return false;
  }
  if (!snapshot_marker->set(snapshot_marker->context, true)) {
    (void)snapshot_marker->set(snapshot_marker->context, false);
    return false;
  }
  if (!close_admission(close_context)) {
    (void)snapshot_marker->set(snapshot_marker->context, false);
    return false;
  }

  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    samples[sensor] = cache->latest[sensor];
    cache->pending[sensor] = false;
    cache->acquisition_overrun_latched[sensor] = false;
  }
  return analyzer_marker_finish(snapshot_marker,
                                IKB_ANALYZER_MARKER_MIN_HIGH_US);
}

bool ikb_analyzer_emit_terminal_marker(
    const ikb_terminal_marker_t *terminal_marker) {
  if (terminal_marker == NULL || terminal_marker->set_both == NULL ||
      terminal_marker->endpoints_idle == NULL ||
      terminal_marker->delay_us == NULL ||
      !terminal_marker->endpoints_idle(terminal_marker->context)) {
    return false;
  }

  terminal_marker->delay_us(terminal_marker->context,
                            IKB_ANALYZER_TERMINAL_IDLE_US);
  if (!terminal_marker->endpoints_idle(terminal_marker->context)) {
    return false;
  }
  if (!terminal_marker->set_both(terminal_marker->context, true)) {
    (void)terminal_marker->set_both(terminal_marker->context, false);
    return false;
  }

  terminal_marker->delay_us(terminal_marker->context,
                            IKB_ANALYZER_TERMINAL_MARKER_US);
  const bool endpoints_idle =
      terminal_marker->endpoints_idle(terminal_marker->context);
  const bool marker_lowered =
      terminal_marker->set_both(terminal_marker->context, false);
  return endpoints_idle && marker_lowered;
}

void ikb_transport_slot_initialize(ikb_transport_slot_t *slot) {
  if (slot != NULL) {
    memset(slot, 0, sizeof(*slot));
  }
}

bool ikb_transport_slot_offer(ikb_transport_slot_t *slot,
                              const uint8_t *bytes, size_t length) {
  if (slot == NULL || bytes == NULL || length == 0U ||
      length > sizeof(slot->bytes) || slot->pending) {
    return false;
  }
  memcpy(slot->bytes, bytes, length);
  slot->length = length;
  slot->offset = 0U;
  slot->pending = true;
  return true;
}

size_t ikb_transport_slot_peek(const ikb_transport_slot_t *slot,
                               const uint8_t **bytes) {
  if (bytes == NULL) {
    return 0U;
  }
  *bytes = NULL;
  if (slot == NULL || !slot->pending || slot->offset >= slot->length) {
    return 0U;
  }
  *bytes = &slot->bytes[slot->offset];
  return slot->length - slot->offset;
}

bool ikb_transport_slot_consume(ikb_transport_slot_t *slot, size_t length) {
  if (slot == NULL || !slot->pending || length == 0U ||
      length > slot->length - slot->offset) {
    return false;
  }
  slot->offset += length;
  if (slot->offset == slot->length) {
    slot->length = 0U;
    slot->offset = 0U;
    slot->pending = false;
  }
  return true;
}

void ikb_transport_slot_drop(ikb_transport_slot_t *slot) {
  ikb_transport_slot_initialize(slot);
}
