#include "controller.h"
#include "glue-policy.h"
#include "identity.h"

#include "firmware/acquisition.h"
#include "firmware/frame-protocol.h"

#include <stdio.h>
#include <string.h>

enum {
  TEST_WHO_AM_I = 0x0f,
  TEST_CTRL1_XL = 0x10,
  TEST_CTRL2_G = 0x11,
  TEST_CTRL3_C = 0x12,
  TEST_DRDY_PULSE_CFG_G = 0x0b,
  TEST_STATUS_REG = 0x1e,
  TEST_OUTX_L_G = 0x22,
  TEST_INT1_CTRL = 0x0d,
};

typedef enum {
  SHUTDOWN_MUX_WRITE = 0,
  SHUTDOWN_MUX_READ = 1,
  SHUTDOWN_REGISTER_WRITE = 2,
  SHUTDOWN_REGISTER_READ = 3,
} shutdown_transaction_kind_t;

typedef struct {
  shutdown_transaction_kind_t kind;
  uint8_t sensor;
  uint8_t register_address;
  uint8_t value;
} shutdown_transaction_t;

#define TEST_MAX_SHUTDOWN_TRANSACTIONS                                      \
  (IKB_SENSOR_SHUTDOWN_TRANSACTION_COUNT + 1U)

typedef struct {
  uint8_t selected_channel;
  uint8_t registers[IKB_SENSOR_COUNT][256];
  uint8_t output_registers[IKB_SENSOR_COUNT][12];
  uint8_t channel_order[64];
  size_t channel_count;
  uint32_t delayed_us;
  uint8_t status_delay_reads[IKB_SENSOR_COUNT];
  bool corrupt_ctrl2_readback;
  bool corrupt_int1_readback;
  bool emit_startup_transcript;
  bool unexpected_register_read;
  bool shutdown_trace_enabled;
  bool shutdown_trace_mismatch;
  shutdown_transaction_t shutdown_expected[TEST_MAX_SHUTDOWN_TRANSACTIONS];
  shutdown_transaction_t shutdown_actual[TEST_MAX_SHUTDOWN_TRANSACTIONS];
  size_t shutdown_expected_count;
  size_t shutdown_actual_count;
  size_t shutdown_fail_index;
} fake_sensor_bus_t;

typedef struct {
  uint8_t next_value;
  bool fail;
} fake_entropy_t;

typedef struct {
  ikb_hal_sample_t samples[IKB_SENSOR_COUNT];
  uint64_t now_us;
} aggregate_context_t;

typedef struct {
  ikb_sample_cache_t *cache;
  ikb_hal_sample_t *samples;
  uint64_t expected_sample_time_us;
  uint8_t events[7];
  size_t event_count;
  bool sensor_pending;
  bool commit_rise_preceded_store;
  bool commit_delay_followed_store;
  bool commit_fall_followed_store;
  bool ready_followed_commit_fall;
  bool snapshot_rise_followed_ready;
  bool snapshot_delay_followed_copy;
  bool snapshot_fall_followed_copy;
} cache_marker_trace_t;

typedef struct {
  bool states[2];
  size_t calls;
  size_t failing_call;
  size_t delay_calls;
  uint32_t delayed_us;
} failing_marker_t;

typedef struct {
  bool asserted;
  bool ready;
  size_t delay_calls;
  uint32_t delayed_us;
} test_marker_t;

typedef struct {
  uint8_t events[4];
  size_t event_count;
  size_t delay_count;
  bool snapshot_asserted;
  bool commit_asserted;
  bool phases_valid;
} startup_anchor_trace_t;

typedef struct {
  startup_anchor_trace_t *trace;
  bool commit_marker;
} startup_marker_context_t;

typedef struct {
  bool endpoint_states[3];
  bool marker_states[2];
  uint32_t delays[2];
  size_t endpoint_calls;
  size_t marker_calls;
  size_t delay_calls;
  size_t failing_marker_call;
} terminal_marker_trace_t;

typedef struct {
  test_marker_t *marker;
  ikb_sample_cache_t *cache;
  size_t calls;
  bool observed_rise_before_copy;
  bool succeeds;
} terminal_close_trace_t;

typedef struct {
  ikb_glue_policy_t *policy;
  ikb_glue_sensor_policy_t *sensors;
} terminal_policy_close_t;

enum {
  COMMIT_MARKER_RISE = 1,
  COMMIT_MARKER_DELAY = 2,
  COMMIT_MARKER_FALL = 3,
  SENSOR_READY = 4,
  SNAPSHOT_MARKER_RISE = 5,
  SNAPSHOT_MARKER_DELAY = 6,
  SNAPSHOT_MARKER_FALL = 7,
};

enum {
  STARTUP_SNAPSHOT_RISE = 1,
  STARTUP_SNAPSHOT_FALL = 2,
  STARTUP_COMMIT_RISE = 3,
  STARTUP_COMMIT_FALL = 4,
};

static bool expect(bool condition, const char *message) {
  if (condition) {
    return true;
  }
  fprintf(stderr, "zephyr native conformance failure: %s\n", message);
  return false;
}

static bool shutdown_transactions_equal(
    const shutdown_transaction_t *left,
    const shutdown_transaction_t *right) {
  return left->kind == right->kind && left->sensor == right->sensor &&
         left->register_address == right->register_address &&
         left->value == right->value;
}

static bool record_shutdown_transaction(fake_sensor_bus_t *bus,
                                         shutdown_transaction_kind_t kind,
                                         uint8_t sensor,
                                         uint8_t register_address,
                                         uint8_t value) {
  if (!bus->shutdown_trace_enabled) {
    return true;
  }
  const size_t index = bus->shutdown_actual_count;
  if (index >= TEST_MAX_SHUTDOWN_TRANSACTIONS) {
    bus->shutdown_trace_mismatch = true;
    return false;
  }
  const shutdown_transaction_t actual = {
      .kind = kind,
      .sensor = sensor,
      .register_address = register_address,
      .value = value,
  };
  bus->shutdown_actual[index] = actual;
  ++bus->shutdown_actual_count;
  if (index >= bus->shutdown_expected_count ||
      !shutdown_transactions_equal(&actual,
                                   &bus->shutdown_expected[index])) {
    bus->shutdown_trace_mismatch = true;
    return false;
  }
  return index != bus->shutdown_fail_index;
}

static bool fake_select_channel(void *context, uint8_t channel) {
  fake_sensor_bus_t *bus = context;
  if (channel >= IKB_SENSOR_COUNT ||
      bus->channel_count >= sizeof(bus->channel_order)) {
    return false;
  }
  bus->selected_channel = channel;
  bus->channel_order[bus->channel_count++] = channel;
  const uint8_t mask = (uint8_t)(1U << channel);
  if (bus->emit_startup_transcript) {
    printf("W 70 %02x\nD 10\nR 70 %02x\n", (unsigned int)mask,
           (unsigned int)mask);
  }
  return record_shutdown_transaction(bus, SHUTDOWN_MUX_WRITE, channel, 0U,
                                     mask) &&
         record_shutdown_transaction(bus, SHUTDOWN_MUX_READ, channel, 0U,
                                     mask);
}

static bool fake_deselect_all(void *context) {
  fake_sensor_bus_t *bus = context;
  return record_shutdown_transaction(bus, SHUTDOWN_MUX_WRITE, UINT8_MAX, 0U,
                                     0U) &&
         record_shutdown_transaction(bus, SHUTDOWN_MUX_READ, UINT8_MAX, 0U,
                                     0U);
}

static bool fake_write_register(void *context, uint8_t register_address,
                                uint8_t value) {
  fake_sensor_bus_t *bus = context;
  uint8_t *registers = bus->registers[bus->selected_channel];
  registers[register_address] = value;
  if (register_address == TEST_CTRL3_C && (value & 0x01U) != 0U) {
    registers[TEST_CTRL3_C] = 0U;
  }
  if (bus->emit_startup_transcript) {
    printf("W 6a %02x %02x\n", (unsigned int)register_address,
           (unsigned int)value);
  }
  return record_shutdown_transaction(
      bus, SHUTDOWN_REGISTER_WRITE, bus->selected_channel,
      register_address, value);
}

static bool fake_read_register(void *context, uint8_t register_address,
                               uint8_t *value) {
  fake_sensor_bus_t *bus = context;
  if (value == NULL) {
    return false;
  }
  switch (register_address) {
    case TEST_WHO_AM_I:
    case TEST_CTRL1_XL:
    case TEST_CTRL2_G:
    case TEST_CTRL3_C:
    case TEST_DRDY_PULSE_CFG_G:
    case TEST_STATUS_REG:
    case TEST_INT1_CTRL:
      break;
    default:
      bus->unexpected_register_read = true;
      return false;
  }
  if (register_address == TEST_STATUS_REG &&
      bus->status_delay_reads[bus->selected_channel] > 0U) {
    --bus->status_delay_reads[bus->selected_channel];
    *value = 0x01U;
    return true;
  }
  *value = bus->registers[bus->selected_channel][register_address];
  if (bus->corrupt_ctrl2_readback && register_address == TEST_CTRL2_G) {
    *value ^= 0x10U;
  }
  if (bus->corrupt_int1_readback && register_address == TEST_INT1_CTRL) {
    *value ^= 0x01U;
  }
  if (bus->emit_startup_transcript) {
    printf("WR 6a %02x %02x\n", (unsigned int)register_address,
           (unsigned int)*value);
  }
  return record_shutdown_transaction(
      bus, SHUTDOWN_REGISTER_READ, bus->selected_channel,
      register_address, *value);
}

static bool fake_read_registers(void *context, uint8_t register_address,
                                uint8_t *values, size_t length) {
  fake_sensor_bus_t *bus = context;
  if (values == NULL || register_address != TEST_OUTX_L_G || length != 12U) {
    return false;
  }
  memcpy(values, bus->output_registers[bus->selected_channel], length);
  return true;
}

static void fake_delay_us(void *context, uint32_t delay_us) {
  fake_sensor_bus_t *bus = context;
  bus->delayed_us += delay_us;
  if (bus->emit_startup_transcript) {
    printf("D %u\n", (unsigned int)delay_us);
  }
}

static ikb_sensor_bus_t fake_bus_interface(fake_sensor_bus_t *bus) {
  return (ikb_sensor_bus_t){
      .context = bus,
      .select_channel = fake_select_channel,
      .deselect_all = fake_deselect_all,
      .write_register = fake_write_register,
      .read_register = fake_read_register,
      .read_registers = fake_read_registers,
      .delay_us = fake_delay_us,
  };
}

static bool fake_random_fill(void *context, uint8_t *output,
                             size_t output_length) {
  fake_entropy_t *entropy = context;
  if (entropy->fail) {
    return false;
  }
  for (size_t index = 0U; index < output_length; ++index) {
    output[index] = entropy->next_value++;
  }
  return true;
}

static void put_i16_le(uint8_t *bytes, size_t offset, int16_t value) {
  const uint16_t encoded = (uint16_t)value;
  bytes[offset] = (uint8_t)(encoded & 0xffU);
  bytes[offset + 1U] = (uint8_t)(encoded >> 8U);
}

static void prepare_fake_bus(fake_sensor_bus_t *bus) {
  memset(bus, 0, sizeof(*bus));
  bus->shutdown_fail_index = SIZE_MAX;
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    bus->registers[sensor][TEST_WHO_AM_I] = IKB_LSM6DS3TR_C_WHO_AM_I;
    bus->registers[sensor][TEST_STATUS_REG] = 0x03U;
    bus->registers[sensor][TEST_DRDY_PULSE_CFG_G] = 0xffU;
    put_i16_le(bus->output_registers[sensor], 0U,
               (int16_t)(100 + sensor));
    put_i16_le(bus->output_registers[sensor], 2U,
               (int16_t)(200 + sensor));
    put_i16_le(bus->output_registers[sensor], 4U,
               (int16_t)(300 + sensor));
    put_i16_le(bus->output_registers[sensor], 6U,
               (int16_t)(-100 - sensor));
    put_i16_le(bus->output_registers[sensor], 8U,
               (int16_t)(-200 - sensor));
    put_i16_le(bus->output_registers[sensor], 10U,
               (int16_t)(-300 - sensor));
  }
}

static bool expect_hex(const uint8_t *actual, size_t actual_length,
                       const char *expected_hex) {
  static const char digits[] = "0123456789abcdef";
  if (strlen(expected_hex) != actual_length * 2U) {
    return false;
  }
  for (size_t index = 0U; index < actual_length; ++index) {
    if (expected_hex[index * 2U] != digits[actual[index] >> 4U] ||
        expected_hex[index * 2U + 1U] != digits[actual[index] & 0x0fU]) {
      return false;
    }
  }
  return true;
}

static bool aggregate_take(void *context, uint8_t sensor_index,
                           ikb_hal_sample_t *sample) {
  const aggregate_context_t *aggregate = context;
  if (aggregate == NULL || sensor_index >= IKB_SENSOR_COUNT || sample == NULL) {
    return false;
  }
  *sample = aggregate->samples[sensor_index];
  return true;
}

static uint64_t aggregate_now_us(void *context) {
  const aggregate_context_t *aggregate = context;
  return aggregate->now_us;
}

static bool set_test_marker(void *context, bool asserted) {
  test_marker_t *marker = context;
  if (marker == NULL) {
    return false;
  }
  marker->asserted = asserted;
  return true;
}

static void delay_test_marker(void *context, uint32_t delay_us) {
  test_marker_t *marker = context;
  if (marker != NULL) {
    ++marker->delay_calls;
    marker->delayed_us += delay_us;
  }
}

static void publish_test_ready(void *context) {
  test_marker_t *marker = context;
  if (marker != NULL) {
    marker->ready = true;
  }
}

static ikb_analyzer_marker_t test_marker_interface(test_marker_t *marker) {
  return (ikb_analyzer_marker_t){
      .context = marker,
      .set = set_test_marker,
      .delay_us = delay_test_marker,
  };
}

static bool commit_for_test(ikb_sample_cache_t *cache, uint8_t sensor_index,
                            const ikb_hal_sample_t *sample) {
  test_marker_t marker = {0};
  const ikb_analyzer_marker_t interface = test_marker_interface(&marker);
  return ikb_sample_cache_commit(cache, sensor_index, sample, &interface,
                                 publish_test_ready, &marker) &&
         !marker.asserted && marker.ready && marker.delay_calls == 1U &&
         marker.delayed_us == IKB_ANALYZER_MARKER_MIN_HIGH_US;
}

static bool snapshot_for_test(
    ikb_sample_cache_t *cache,
    ikb_hal_sample_t samples[IKB_SENSOR_COUNT]) {
  test_marker_t marker = {0};
  const ikb_analyzer_marker_t interface = test_marker_interface(&marker);
  return ikb_sample_cache_snapshot(cache, samples, &interface) &&
         !marker.asserted && marker.delay_calls == 1U &&
         marker.delayed_us == IKB_ANALYZER_MARKER_MIN_HIGH_US;
}

static bool trace_commit_marker(void *context, bool asserted) {
  cache_marker_trace_t *trace = context;
  if (trace == NULL || trace->event_count >= 7U) {
    return false;
  }
  if (asserted) {
    trace->commit_rise_preceded_store =
        trace->event_count == 0U && trace->sensor_pending &&
        !trace->cache->pending[0] &&
        trace->cache->latest[0].sample_time_us == 0U;
    trace->events[trace->event_count++] = COMMIT_MARKER_RISE;
  } else {
    trace->commit_fall_followed_store =
        trace->event_count == 2U &&
        trace->events[trace->event_count - 1U] == COMMIT_MARKER_DELAY &&
        trace->sensor_pending && trace->cache->pending[0] &&
        trace->cache->latest[0].sample_time_us ==
            trace->expected_sample_time_us;
    trace->events[trace->event_count++] = COMMIT_MARKER_FALL;
  }
  return true;
}

static void trace_commit_delay(void *context, uint32_t delay_us) {
  cache_marker_trace_t *trace = context;
  if (trace == NULL || trace->event_count >= 7U) {
    return;
  }
  trace->commit_delay_followed_store =
      delay_us == IKB_ANALYZER_MARKER_MIN_HIGH_US &&
      trace->event_count == 1U &&
      trace->events[trace->event_count - 1U] == COMMIT_MARKER_RISE &&
      trace->sensor_pending && trace->cache->pending[0] &&
      trace->cache->latest[0].sample_time_us ==
          trace->expected_sample_time_us;
  trace->events[trace->event_count++] = COMMIT_MARKER_DELAY;
}

static void trace_sensor_ready(void *context) {
  cache_marker_trace_t *trace = context;
  if (trace == NULL || trace->event_count >= 7U) {
    return;
  }
  trace->ready_followed_commit_fall =
      trace->event_count == 3U && trace->sensor_pending &&
      trace->events[trace->event_count - 1U] == COMMIT_MARKER_FALL;
  trace->sensor_pending = false;
  trace->events[trace->event_count++] = SENSOR_READY;
}

static bool trace_snapshot_marker(void *context, bool asserted) {
  cache_marker_trace_t *trace = context;
  if (trace == NULL || trace->event_count >= 7U) {
    return false;
  }
  if (asserted) {
    trace->snapshot_rise_followed_ready =
        trace->event_count == 4U && !trace->sensor_pending &&
        trace->events[trace->event_count - 1U] == SENSOR_READY &&
        trace->cache->pending[0] &&
        trace->samples[0].sample_time_us == 0U;
    trace->events[trace->event_count++] = SNAPSHOT_MARKER_RISE;
  } else {
    trace->snapshot_fall_followed_copy =
        trace->event_count == 6U &&
        trace->events[trace->event_count - 1U] == SNAPSHOT_MARKER_DELAY &&
        !trace->cache->pending[0] &&
        trace->samples[0].sample_time_us ==
            trace->expected_sample_time_us;
    trace->events[trace->event_count++] = SNAPSHOT_MARKER_FALL;
  }
  return true;
}

static void trace_snapshot_delay(void *context, uint32_t delay_us) {
  cache_marker_trace_t *trace = context;
  if (trace == NULL || trace->event_count >= 7U) {
    return;
  }
  trace->snapshot_delay_followed_copy =
      delay_us == IKB_ANALYZER_MARKER_MIN_HIGH_US &&
      trace->event_count == 5U &&
      trace->events[trace->event_count - 1U] == SNAPSHOT_MARKER_RISE &&
      !trace->cache->pending[0] &&
      trace->samples[0].sample_time_us ==
          trace->expected_sample_time_us;
  trace->events[trace->event_count++] = SNAPSHOT_MARKER_DELAY;
}

static bool fail_selected_marker_call(void *context, bool asserted) {
  failing_marker_t *marker = context;
  if (marker == NULL || marker->calls >= 2U) {
    return false;
  }
  const size_t call = marker->calls++;
  marker->states[call] = asserted;
  return call != marker->failing_call;
}

static void delay_failing_marker(void *context, uint32_t delay_us) {
  failing_marker_t *marker = context;
  if (marker != NULL) {
    ++marker->delay_calls;
    marker->delayed_us += delay_us;
  }
}

static ikb_analyzer_marker_t failing_marker_interface(
    failing_marker_t *marker) {
  return (ikb_analyzer_marker_t){
      .context = marker,
      .set = fail_selected_marker_call,
      .delay_us = delay_failing_marker,
  };
}

static bool trace_startup_marker(void *context, bool asserted) {
  startup_marker_context_t *marker = context;
  if (marker == NULL || marker->trace == NULL ||
      marker->trace->event_count >= 4U) {
    return false;
  }
  startup_anchor_trace_t *trace = marker->trace;
  bool *state = marker->commit_marker ? &trace->commit_asserted
                                      : &trace->snapshot_asserted;
  const bool other_state = marker->commit_marker ? trace->snapshot_asserted
                                                  : trace->commit_asserted;
  trace->phases_valid &= asserted ? !*state && !other_state
                                   : *state && !other_state;
  *state = asserted;
  if (marker->commit_marker) {
    trace->events[trace->event_count++] =
        asserted ? STARTUP_COMMIT_RISE : STARTUP_COMMIT_FALL;
  } else {
    trace->events[trace->event_count++] =
        asserted ? STARTUP_SNAPSHOT_RISE : STARTUP_SNAPSHOT_FALL;
  }
  return true;
}

static void trace_startup_delay(void *context, uint32_t delay_us) {
  startup_marker_context_t *marker = context;
  if (marker == NULL || marker->trace == NULL) {
    return;
  }
  startup_anchor_trace_t *trace = marker->trace;
  bool phase_state_valid = false;
  switch (trace->delay_count) {
    case 0U:
      phase_state_valid = trace->snapshot_asserted &&
                          !trace->commit_asserted;
      break;
    case 1U:
      phase_state_valid = !trace->snapshot_asserted &&
                          !trace->commit_asserted;
      break;
    case 2U:
      phase_state_valid = !trace->snapshot_asserted &&
                          trace->commit_asserted;
      break;
    case 3U:
      phase_state_valid = !trace->snapshot_asserted &&
                          !trace->commit_asserted;
      break;
    default:
      break;
  }
  trace->phases_valid &=
      phase_state_valid && delay_us == IKB_ANALYZER_STARTUP_PHASE_US;
  ++trace->delay_count;
}

static void append_shutdown_expected(fake_sensor_bus_t *bus,
                                     shutdown_transaction_kind_t kind,
                                     uint8_t sensor,
                                     uint8_t register_address,
                                     uint8_t value) {
  if (bus->shutdown_expected_count >= TEST_MAX_SHUTDOWN_TRANSACTIONS) {
    bus->shutdown_trace_mismatch = true;
    return;
  }
  bus->shutdown_expected[bus->shutdown_expected_count++] =
      (shutdown_transaction_t){
          .kind = kind,
          .sensor = sensor,
          .register_address = register_address,
          .value = value,
      };
}

static void append_mux_pair(fake_sensor_bus_t *bus, uint8_t sensor,
                            uint8_t mask) {
  append_shutdown_expected(bus, SHUTDOWN_MUX_WRITE, sensor, 0U, mask);
  append_shutdown_expected(bus, SHUTDOWN_MUX_READ, sensor, 0U, mask);
}

static void build_shutdown_expected(fake_sensor_bus_t *bus) {
  bus->shutdown_expected_count = 0U;
  append_mux_pair(bus, UINT8_MAX, 0U);
  static const uint8_t shutdown_registers[] = {
      TEST_INT1_CTRL,
      TEST_CTRL1_XL,
      TEST_CTRL2_G,
  };
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    append_mux_pair(bus, sensor, (uint8_t)(1U << sensor));
    for (size_t index = 0U; index < sizeof(shutdown_registers); ++index) {
      append_shutdown_expected(bus, SHUTDOWN_REGISTER_WRITE, sensor,
                               shutdown_registers[index], 0U);
      append_shutdown_expected(bus, SHUTDOWN_REGISTER_READ, sensor,
                               shutdown_registers[index], 0U);
    }
  }
  append_mux_pair(bus, UINT8_MAX, 0U);
}

static void begin_shutdown_trace(fake_sensor_bus_t *bus) {
  bus->shutdown_trace_enabled = true;
  bus->shutdown_trace_mismatch = false;
  bus->shutdown_actual_count = 0U;
  bus->shutdown_fail_index = SIZE_MAX;
  build_shutdown_expected(bus);
}

static bool shutdown_trace_matches(fake_sensor_bus_t *bus) {
  const ikb_sensor_bus_t interface = fake_bus_interface(bus);
  return ikb_sensor_bank_shutdown(&interface) &&
         !bus->shutdown_trace_mismatch &&
         bus->shutdown_actual_count == bus->shutdown_expected_count;
}

static void begin_arm_trace(fake_sensor_bus_t *bus) {
  bus->shutdown_trace_enabled = true;
  bus->shutdown_expected_count = 0U;
  bus->shutdown_actual_count = 0U;
  bus->shutdown_trace_mismatch = false;
  bus->shutdown_fail_index = SIZE_MAX;
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    append_mux_pair(bus, sensor, (uint8_t)(1U << sensor));
    append_shutdown_expected(bus, SHUTDOWN_REGISTER_WRITE, sensor,
                             TEST_INT1_CTRL, 0x01U);
    append_shutdown_expected(bus, SHUTDOWN_REGISTER_READ, sensor,
                             TEST_INT1_CTRL, 0x01U);
  }
}

static bool terminal_trace_endpoints_idle(void *context) {
  terminal_marker_trace_t *trace = context;
  if (trace == NULL || trace->endpoint_calls >= 3U) {
    return false;
  }
  return trace->endpoint_states[trace->endpoint_calls++];
}

static bool terminal_trace_set_both(void *context, bool asserted) {
  terminal_marker_trace_t *trace = context;
  if (trace == NULL || trace->marker_calls >= 2U) {
    return false;
  }
  const size_t call = trace->marker_calls++;
  trace->marker_states[call] = asserted;
  return call != trace->failing_marker_call;
}

static void terminal_trace_delay(void *context, uint32_t delay_us) {
  terminal_marker_trace_t *trace = context;
  if (trace != NULL && trace->delay_calls < 2U) {
    trace->delays[trace->delay_calls++] = delay_us;
  }
}

static ikb_terminal_marker_t terminal_trace_interface(
    terminal_marker_trace_t *trace) {
  return (ikb_terminal_marker_t){
      .context = trace,
      .set_both = terminal_trace_set_both,
      .endpoints_idle = terminal_trace_endpoints_idle,
      .delay_us = terminal_trace_delay,
  };
}

static bool trace_terminal_close(void *context) {
  terminal_close_trace_t *trace = context;
  if (trace == NULL || trace->marker == NULL || trace->cache == NULL) {
    return false;
  }
  ++trace->calls;
  trace->observed_rise_before_copy = trace->marker->asserted;
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    trace->observed_rise_before_copy &= trace->cache->pending[sensor];
  }
  return trace->succeeds;
}

static bool close_terminal_policy(void *context) {
  terminal_policy_close_t *close = context;
  if (close == NULL || close->policy == NULL || close->sensors == NULL ||
      !ikb_glue_close_boundary(close->policy)) {
    return false;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!ikb_glue_sensor_close(&close->sensors[sensor])) {
      ikb_glue_policy_fail(close->policy);
      return false;
    }
  }
  return true;
}

static bool consume_transport_record(ikb_glue_policy_t *policy,
                                     const uint8_t *expected,
                                     size_t expected_length) {
  const uint8_t *pending = NULL;
  return ikb_glue_transport_peek(policy, &pending) == expected_length &&
         pending != NULL &&
         memcmp(pending, expected, expected_length) == 0 &&
         ikb_glue_transport_written(policy, (int)expected_length) ==
             IKB_GLUE_TRANSPORT_COMPLETE;
}

static bool run_integrated_quiesce_path(
    const uint8_t controller_id[IKB_CONTROLLER_ID_SHA256_LEN],
    const uint8_t boot_id[IKB_BOOT_ID_LEN]) {
  /* This composes the same portable helpers as main.c. Only GPIO, I2C, and
   * time terminate in transcript fixtures.
   */
  const uint64_t target_time_us = 900000U;
  const uint64_t request_time_us = 800000U;
  ikb_glue_policy_t policy;
  ikb_glue_policy_initialize(&policy);
  ikb_glue_sensor_policy_t sensors[IKB_SENSOR_COUNT];
  ikb_sample_cache_t cache;
  ikb_sample_cache_initialize(&cache);
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    ikb_glue_sensor_initialize(&sensors[sensor]);
    const ikb_hal_sample_t sample = {
        .sample_time_us = target_time_us - sensor,
        .fresh = true,
    };
    if (!commit_for_test(&cache, sensor, &sample)) {
      return false;
    }
  }

  ikb_quiesce_request_t request = {
      .hand = IKB_HAND_LEFT,
      .request_start_time_ns = 0x0102030405060708ULL,
  };
  memcpy(request.controller_id_sha256, controller_id,
         sizeof(request.controller_id_sha256));
  memcpy(request.boot_id, boot_id, sizeof(request.boot_id));
  memset(request.session_id, 0x5a, sizeof(request.session_id));
  const uint8_t older_record[] = {0xa0U, 0xa1U, 0xa2U};
  uint32_t elapsed_periods = 0U;
  if (!ikb_glue_transport_offer(&policy, older_record,
                                sizeof(older_record)) ||
      !ikb_glue_request_quiesce(&policy, &request, IKB_HAND_LEFT,
                                controller_id, boot_id, request_time_us) ||
      !ikb_glue_timer_elapsed(&policy) ||
      !ikb_glue_aggregate_begin(&policy, &elapsed_periods) ||
      elapsed_periods != 1U ||
      !ikb_glue_final_boundary_ready(
          &policy, request_time_us + 1U,
          ikb_sample_cache_terminal_ready(&cache), true, true)) {
    return false;
  }

  test_marker_t snapshot_marker = {0};
  const ikb_analyzer_marker_t snapshot_interface =
      test_marker_interface(&snapshot_marker);
  terminal_policy_close_t close = {
      .policy = &policy,
      .sensors = sensors,
  };
  ikb_hal_sample_t samples[IKB_SENSOR_COUNT] = {0};
  if (!ikb_sample_cache_terminal_snapshot(
          &cache, samples, &snapshot_interface, close_terminal_policy,
          &close) ||
      ikb_glue_policy_lifecycle(&policy) != IKB_GLUE_BOUNDARY_CLOSED) {
    return false;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (ikb_glue_sensor_state(&sensors[sensor]) !=
            IKB_GLUE_SENSOR_CLOSED ||
        !samples[sensor].fresh) {
      return false;
    }
  }

  aggregate_context_t aggregate = {
      .now_us = target_time_us + 100U,
  };
  memcpy(aggregate.samples, samples, sizeof(samples));
  const ikb_acquisition_hal_t hal = {
      .context = &aggregate,
      .read_latest = aggregate_take,
      .now_us = aggregate_now_us,
  };
  ikb_acquisition_state_t acquisition;
  ikb_sensor_frame_t final_frame;
  uint8_t final_transport[IKB_TRANSPORT_MAX_LEN];
  if (!ikb_acquisition_init(&acquisition, IKB_HAND_LEFT, controller_id,
                            boot_id) ||
      !ikb_acquire_frame(&acquisition, &hal, target_time_us,
                         target_time_us + IKB_AGGREGATE_PERIOD_US,
                         &final_frame) ||
      final_frame.sequence != 0U || final_frame.flags != 0U ||
      final_frame.valid_sensor_mask != IKB_ALL_SENSORS_VALID) {
    return false;
  }
  const size_t final_length = ikb_encode_transport_frame(
      &final_frame, final_transport, sizeof(final_transport));
  if (final_length == 0U ||
      !ikb_glue_transport_offer_final(&policy, final_transport,
                                      final_length) ||
      !ikb_glue_aggregate_finish(&policy) ||
      !ikb_glue_begin_shutdown(&policy)) {
    return false;
  }

  fake_sensor_bus_t shutdown_bus;
  prepare_fake_bus(&shutdown_bus);
  begin_shutdown_trace(&shutdown_bus);
  terminal_marker_trace_t marker_trace = {
      .endpoint_states = {true, true, true},
      .failing_marker_call = SIZE_MAX,
  };
  ikb_terminal_marker_t marker = terminal_trace_interface(&marker_trace);
  if (!shutdown_trace_matches(&shutdown_bus) ||
      !ikb_glue_begin_marking(&policy, request_time_us + 200U) ||
      !ikb_analyzer_emit_terminal_marker(&marker) ||
      !ikb_glue_marker_fell(&policy, request_time_us + 600U)) {
    return false;
  }

  ikb_quiesce_receipt_t receipt = {
      .hand = request.hand,
      .request_start_time_ns = request.request_start_time_ns,
      .final_sequence = final_frame.sequence,
  };
  memcpy(receipt.controller_id_sha256, request.controller_id_sha256,
         sizeof(receipt.controller_id_sha256));
  memcpy(receipt.boot_id, request.boot_id, sizeof(receipt.boot_id));
  memcpy(receipt.session_id, request.session_id, sizeof(receipt.session_id));
  uint8_t receipt_transport[IKB_QUIESCE_RECEIPT_TRANSPORT_LEN];
  const size_t receipt_length = ikb_encode_transport_quiesce_receipt(
      &receipt, receipt_transport, sizeof(receipt_transport));
  if (receipt_length != IKB_QUIESCE_RECEIPT_TRANSPORT_LEN ||
      !ikb_glue_transport_offer_receipt(
          &policy, receipt_transport, receipt_length, request_time_us + 700U)) {
    return false;
  }

  uint8_t decoded[IKB_QUIESCE_RECEIPT_LEN];
  ikb_quiesce_receipt_t observed;
  if (ikb_decode_transport_record(receipt_transport, receipt_length - 1U,
                                  decoded, sizeof(decoded)) !=
          sizeof(decoded) ||
      !ikb_decode_quiesce_receipt(decoded, sizeof(decoded), &observed) ||
      observed.final_sequence != final_frame.sequence ||
      memcmp(observed.session_id, request.session_id,
             sizeof(observed.session_id)) != 0) {
    return false;
  }

  const uint8_t *pending = NULL;
  return consume_transport_record(&policy, older_record,
                                  sizeof(older_record)) &&
         consume_transport_record(&policy, final_transport, final_length) &&
         consume_transport_record(&policy, receipt_transport,
                                  receipt_length) &&
         ikb_glue_transport_peek(&policy, &pending) == 0U &&
         pending == NULL &&
         ikb_glue_policy_lifecycle(&policy) == IKB_GLUE_QUIESCED &&
         !ikb_glue_policy_failed(&policy);
}

int main(int argc, char **argv) {
  if (argc == 2 && strcmp(argv[1], "--startup-transcript") == 0) {
    fake_sensor_bus_t transcript_bus;
    prepare_fake_bus(&transcript_bus);
    transcript_bus.emit_startup_transcript = true;
    const ikb_sensor_bus_t interface = fake_bus_interface(&transcript_bus);
    return ikb_sensor_bank_initialize(&interface) &&
                   ikb_sensor_bank_arm(&interface) && !ferror(stdout)
               ? 0
               : 1;
  }
  if (argc != 1) {
    fprintf(stderr, "expected no arguments or --startup-transcript\n");
    return 1;
  }
  startup_anchor_trace_t startup_trace = {
      .phases_valid = true,
  };
  startup_marker_context_t startup_snapshot_context = {
      .trace = &startup_trace,
  };
  startup_marker_context_t startup_commit_context = {
      .trace = &startup_trace,
      .commit_marker = true,
  };
  const ikb_analyzer_marker_t startup_snapshot_marker = {
      .context = &startup_snapshot_context,
      .set = trace_startup_marker,
      .delay_us = trace_startup_delay,
  };
  const ikb_analyzer_marker_t startup_commit_marker = {
      .context = &startup_commit_context,
      .set = trace_startup_marker,
      .delay_us = trace_startup_delay,
  };
  if (!expect(IKB_ANALYZER_MARKER_MIN_HIGH_US == 2U,
              "normal analyzer markers request at least two microseconds") ||
      !expect(IKB_ANALYZER_STARTUP_PHASE_US == 20U,
              "startup anchor uses distinctive twenty-microsecond phases") ||
      !expect(ikb_analyzer_emit_startup_anchor(&startup_snapshot_marker,
                                               &startup_commit_marker),
              "startup anchor emits before sensor acquisition") ||
      !expect(startup_trace.event_count == 4U &&
                  startup_trace.delay_count == 4U &&
                  startup_trace.phases_valid &&
                  startup_trace.events[0] == STARTUP_SNAPSHOT_RISE &&
                  startup_trace.events[1] == STARTUP_SNAPSHOT_FALL &&
                  startup_trace.events[2] == STARTUP_COMMIT_RISE &&
                  startup_trace.events[3] == STARTUP_COMMIT_FALL &&
                  !startup_trace.snapshot_asserted &&
                  !startup_trace.commit_asserted,
              "startup anchor is D8 high, idle, D9 high, idle")) {
    return 1;
  }

  failing_marker_t startup_assertion_failure = {
      .failing_call = 0U,
  };
  const ikb_analyzer_marker_t failed_startup_snapshot_marker =
      failing_marker_interface(&startup_assertion_failure);
  test_marker_t unreached_startup_commit = {0};
  const ikb_analyzer_marker_t unreached_startup_commit_marker =
      test_marker_interface(&unreached_startup_commit);
  if (!expect(!ikb_analyzer_emit_startup_anchor(
                  &failed_startup_snapshot_marker,
                  &unreached_startup_commit_marker),
              "startup marker GPIO failure fails boot evidence closed") ||
      !expect(startup_assertion_failure.calls == 1U &&
                  startup_assertion_failure.delay_calls == 0U &&
                  !unreached_startup_commit.asserted &&
                  unreached_startup_commit.delay_calls == 0U,
              "failed startup prefix cannot continue as a partial anchor")) {
    return 1;
  }

  fake_sensor_bus_t fake_bus;
  prepare_fake_bus(&fake_bus);
  const ikb_sensor_bus_t bus = fake_bus_interface(&fake_bus);

  if (!expect(IKB_AGGREGATE_PERIOD_US == 1000000U / IKB_AGGREGATE_RATE_HZ,
              "200 Hz aggregate period is exact") ||
      !expect(IKB_SENSOR_ODR_HZ == 208U, "sensor ODR is 208 Hz") ||
      !expect(IKB_SENSOR_PRIMING_TIMEOUT_MS == 100U,
              "startup clean-cache priming has a fixed 100 ms cap") ||
      !expect(ikb_sensor_bank_initialize(&bus),
              "six-sensor bank initializes with readback")) {
    return 1;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!expect(fake_bus.channel_order[sensor] == sensor,
                "sensor initialization uses canonical mux order") ||
        !expect(fake_bus.registers[sensor][TEST_CTRL1_XL] == 0x58U,
                "accelerometer is 208 Hz and +/-4g") ||
        !expect(fake_bus.registers[sensor][TEST_CTRL2_G] == 0x58U,
                "gyroscope is 208 Hz and +/-1000 dps") ||
        !expect(fake_bus.registers[sensor][TEST_CTRL3_C] == 0x44U,
                "BDU and register auto-increment are enabled") ||
        !expect(fake_bus.registers[sensor][TEST_DRDY_PULSE_CFG_G] == 0x80U,
                "each conversion emits a distinct data-ready pulse") ||
        !expect(fake_bus.registers[sensor][TEST_INT1_CTRL] == 0x00U,
                "all sensor INT1 outputs stay quiet until callbacks exist")) {
      return 1;
    }
  }

  if (!expect(ikb_sensor_bank_arm(&bus),
              "configured bank arms after all callbacks are installed")) {
    return 1;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!expect(fake_bus.channel_order[IKB_SENSOR_COUNT + sensor] == sensor,
                "bank arming reselects every channel in canonical order") ||
        !expect(fake_bus.registers[sensor][TEST_INT1_CTRL] == 0x01U,
                "arming enables every accelerometer data-ready output")) {
      return 1;
    }
  }
  for (size_t mutation = 0U; mutation < 4U * IKB_SENSOR_COUNT; ++mutation) {
    fake_sensor_bus_t nack_arm_bus;
    prepare_fake_bus(&nack_arm_bus);
    begin_arm_trace(&nack_arm_bus);
    nack_arm_bus.shutdown_fail_index = mutation;
    const ikb_sensor_bus_t interface = fake_bus_interface(&nack_arm_bus);
    if (!expect(!ikb_sensor_bank_arm(&interface),
                "every NACKed arming transaction fails closed") ||
        !expect(nack_arm_bus.shutdown_actual_count == mutation + 1U,
                "failed arming stops before touching another transaction")) {
      return 1;
    }
  }
  fake_sensor_bus_t corrupt_arm_bus;
  prepare_fake_bus(&corrupt_arm_bus);
  corrupt_arm_bus.corrupt_int1_readback = true;
  const ikb_sensor_bus_t corrupt_arm_interface =
      fake_bus_interface(&corrupt_arm_bus);
  if (!expect(!ikb_sensor_bank_arm(&corrupt_arm_interface),
              "INT1 arming readback mismatch fails closed")) {
    return 1;
  }

  fake_sensor_bus_t bad_bus;
  prepare_fake_bus(&bad_bus);
  bad_bus.corrupt_ctrl2_readback = true;
  const ikb_sensor_bus_t corrupt_bus = fake_bus_interface(&bad_bus);
  if (!expect(!ikb_sensor_bank_initialize(&corrupt_bus),
              "configuration readback mismatch fails closed")) {
    return 1;
  }

  fake_sensor_bus_t shutdown_bus;
  prepare_fake_bus(&shutdown_bus);
  begin_shutdown_trace(&shutdown_bus);
  if (!expect(shutdown_bus.shutdown_expected_count ==
                  IKB_SENSOR_SHUTDOWN_TRANSACTION_COUNT,
              "shutdown fixture contains exactly 52 transactions") ||
      !expect(shutdown_trace_matches(&shutdown_bus),
              "production shutdown emits the exact ordered transcript")) {
    return 1;
  }

  for (size_t mutation = 0U;
       mutation < IKB_SENSOR_SHUTDOWN_TRANSACTION_COUNT; ++mutation) {
    fake_sensor_bus_t nack_bus;
    prepare_fake_bus(&nack_bus);
    begin_shutdown_trace(&nack_bus);
    nack_bus.shutdown_fail_index = mutation;
    if (!expect(!shutdown_trace_matches(&nack_bus),
                "every NACKed shutdown transaction fails closed")) {
      return 1;
    }

    fake_sensor_bus_t omitted_bus;
    prepare_fake_bus(&omitted_bus);
    begin_shutdown_trace(&omitted_bus);
    memmove(&omitted_bus.shutdown_expected[mutation],
            &omitted_bus.shutdown_expected[mutation + 1U],
            (omitted_bus.shutdown_expected_count - mutation - 1U) *
                sizeof(omitted_bus.shutdown_expected[0]));
    --omitted_bus.shutdown_expected_count;
    if (!expect(!shutdown_trace_matches(&omitted_bus),
                "every omitted shutdown transaction is rejected")) {
      return 1;
    }

    fake_sensor_bus_t duplicated_bus;
    prepare_fake_bus(&duplicated_bus);
    begin_shutdown_trace(&duplicated_bus);
    memmove(&duplicated_bus.shutdown_expected[mutation + 2U],
            &duplicated_bus.shutdown_expected[mutation + 1U],
            (duplicated_bus.shutdown_expected_count - mutation - 1U) *
                sizeof(duplicated_bus.shutdown_expected[0]));
    duplicated_bus.shutdown_expected[mutation + 1U] =
        duplicated_bus.shutdown_expected[mutation];
    ++duplicated_bus.shutdown_expected_count;
    if (!expect(!shutdown_trace_matches(&duplicated_bus),
                "every duplicated shutdown transaction is rejected")) {
      return 1;
    }

    fake_sensor_bus_t reordered_bus;
    prepare_fake_bus(&reordered_bus);
    begin_shutdown_trace(&reordered_bus);
    const size_t neighbor =
        mutation + 1U < IKB_SENSOR_SHUTDOWN_TRANSACTION_COUNT
            ? mutation + 1U
            : mutation - 1U;
    const shutdown_transaction_t swapped =
        reordered_bus.shutdown_expected[mutation];
    reordered_bus.shutdown_expected[mutation] =
        reordered_bus.shutdown_expected[neighbor];
    reordered_bus.shutdown_expected[neighbor] = swapped;
    if (!expect(!shutdown_trace_matches(&reordered_bus),
                "every reordered shutdown transaction is rejected")) {
      return 1;
    }

    fake_sensor_bus_t corrupted_bus;
    prepare_fake_bus(&corrupted_bus);
    begin_shutdown_trace(&corrupted_bus);
    corrupted_bus.shutdown_expected[mutation].value ^= 0x80U;
    if (!expect(!shutdown_trace_matches(&corrupted_bus),
                "every corrupted shutdown transaction is rejected")) {
      return 1;
    }

    fake_sensor_bus_t truncated_bus;
    prepare_fake_bus(&truncated_bus);
    begin_shutdown_trace(&truncated_bus);
    truncated_bus.shutdown_expected_count = mutation;
    if (!expect(!shutdown_trace_matches(&truncated_bus),
                "every truncated shutdown prefix is rejected")) {
      return 1;
    }
  }

  terminal_marker_trace_t terminal_trace = {
      .endpoint_states = {true, true, true},
      .failing_marker_call = SIZE_MAX,
  };
  ikb_terminal_marker_t terminal_interface =
      terminal_trace_interface(&terminal_trace);
  if (!expect(ikb_analyzer_emit_terminal_marker(&terminal_interface),
              "terminal marker requires stable idle endpoints") ||
      !expect(terminal_trace.endpoint_calls == 3U &&
                  terminal_trace.marker_calls == 2U &&
                  terminal_trace.marker_states[0] &&
                  !terminal_trace.marker_states[1] &&
                  terminal_trace.delay_calls == 2U &&
                  terminal_trace.delays[0] ==
                      IKB_ANALYZER_TERMINAL_IDLE_US &&
                  terminal_trace.delays[1] ==
                      IKB_ANALYZER_TERMINAL_MARKER_US,
              "terminal marker is idle, simultaneous rise, hold, and fall")) {
    return 1;
  }

  terminal_marker_trace_t late_endpoint_fault = {
      .endpoint_states = {true, true, false},
      .failing_marker_call = SIZE_MAX,
  };
  terminal_interface = terminal_trace_interface(&late_endpoint_fault);
  if (!expect(!ikb_analyzer_emit_terminal_marker(&terminal_interface) &&
                  late_endpoint_fault.marker_calls == 2U &&
                  late_endpoint_fault.marker_states[0] &&
                  !late_endpoint_fault.marker_states[1],
              "endpoint activity during the marker still forces both low")) {
    return 1;
  }

  terminal_marker_trace_t rise_failure = {
      .endpoint_states = {true, true, true},
      .failing_marker_call = 0U,
  };
  terminal_interface = terminal_trace_interface(&rise_failure);
  if (!expect(!ikb_analyzer_emit_terminal_marker(&terminal_interface) &&
                  rise_failure.marker_calls == 2U &&
                  rise_failure.marker_states[0] &&
                  !rise_failure.marker_states[1],
              "failed simultaneous rise attempts one simultaneous cleanup")) {
    return 1;
  }

  const uint64_t target_time_us = 500000U;
  ikb_sample_cache_t cache;
  ikb_sample_cache_initialize(&cache);

  ikb_sample_cache_t priming_cache;
  ikb_sample_cache_initialize(&priming_cache);
  if (!expect(!ikb_sample_cache_terminal_ready(&priming_cache) &&
                  !ikb_sample_cache_acquisition_faulted(&priming_cache),
              "empty startup cache waits without inventing a fault")) {
    return 1;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    const ikb_hal_sample_t sample = {
        .sample_time_us = target_time_us + sensor,
        .fresh = true,
    };
    if (!expect(commit_for_test(&priming_cache, sensor, &sample),
                "priming uses the ordinary D9 cache commit") ||
        !expect(ikb_sample_cache_terminal_ready(&priming_cache) ==
                    (sensor + 1U == IKB_SENSOR_COUNT),
                "priming waits for all six clean samples") ||
        !expect(!ikb_sample_cache_acquisition_faulted(&priming_cache),
                "clean priming commits cannot create a fault")) {
      return 1;
    }
  }
  const ikb_hal_sample_t priming_replacement = {
      .sample_time_us = target_time_us + 5000U,
      .fresh = true,
  };
  if (!expect(commit_for_test(&priming_cache, 0U, &priming_replacement) &&
                  ikb_sample_cache_terminal_ready(&priming_cache) &&
                  !ikb_sample_cache_acquisition_faulted(&priming_cache),
              "pre-snapshot latest-sample replacement stays clean")) {
    return 1;
  }
  ikb_sample_cache_note_acquisition_fault(&priming_cache, 0U);
  if (!expect(commit_for_test(&priming_cache, 0U, &priming_replacement) &&
                  !ikb_sample_cache_terminal_ready(&priming_cache) &&
                  ikb_sample_cache_acquisition_faulted(&priming_cache),
              "replacement cannot erase a real priming acquisition fault")) {
    return 1;
  }

  ikb_sample_cache_t terminal_cache;
  ikb_sample_cache_initialize(&terminal_cache);
  if (!expect(!ikb_sample_cache_terminal_ready(&terminal_cache),
              "empty cache cannot close the final boundary")) {
    return 1;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    ikb_hal_sample_t terminal_sample = {
        .sample_time_us = target_time_us - sensor,
        .fresh = true,
    };
    if (!expect(commit_for_test(&terminal_cache, sensor, &terminal_sample),
                "terminal fixture commits each clean cache slot")) {
      return 1;
    }
  }
  if (!expect(ikb_sample_cache_terminal_ready(&terminal_cache),
              "six fresh clean samples arm the final boundary")) {
    return 1;
  }
  test_marker_t terminal_snapshot_marker = {0};
  const ikb_analyzer_marker_t terminal_snapshot_marker_interface =
      test_marker_interface(&terminal_snapshot_marker);
  terminal_close_trace_t terminal_close_trace = {
      .marker = &terminal_snapshot_marker,
      .cache = &terminal_cache,
      .succeeds = true,
  };
  ikb_hal_sample_t terminal_samples[IKB_SENSOR_COUNT] = {0};
  if (!expect(ikb_sample_cache_terminal_snapshot(
                  &terminal_cache, terminal_samples,
                  &terminal_snapshot_marker_interface, trace_terminal_close,
                  &terminal_close_trace),
              "final snapshot closes admission inside ordinary D8") ||
      !expect(terminal_close_trace.calls == 1U &&
                  terminal_close_trace.observed_rise_before_copy &&
                  !terminal_snapshot_marker.asserted &&
                  terminal_snapshot_marker.delayed_us ==
                      IKB_ANALYZER_MARKER_MIN_HIGH_US,
              "D8 rises before closure and falls after the cache copy")) {
    return 1;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!expect(terminal_samples[sensor].fresh &&
                    terminal_samples[sensor].sample_time_us ==
                        target_time_us - sensor &&
                    !terminal_cache.pending[sensor],
                "final snapshot consumes each exact clean slot")) {
      return 1;
    }
  }

  ikb_sample_cache_t refused_terminal_cache;
  ikb_sample_cache_initialize(&refused_terminal_cache);
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    ikb_hal_sample_t terminal_sample = {
        .sample_time_us = target_time_us,
        .fresh = true,
    };
    if (!commit_for_test(&refused_terminal_cache, sensor, &terminal_sample)) {
      return 1;
    }
  }
  test_marker_t refused_terminal_marker = {0};
  const ikb_analyzer_marker_t refused_terminal_marker_interface =
      test_marker_interface(&refused_terminal_marker);
  terminal_close_trace_t refused_close = {
      .marker = &refused_terminal_marker,
      .cache = &refused_terminal_cache,
  };
  if (!expect(!ikb_sample_cache_terminal_snapshot(
                  &refused_terminal_cache, terminal_samples,
                  &refused_terminal_marker_interface, trace_terminal_close,
                  &refused_close) &&
                  refused_close.calls == 1U &&
                  refused_close.observed_rise_before_copy &&
                  !refused_terminal_marker.asserted,
              "failed admission closure lowers D8 without consuming cache") ||
      !expect(ikb_sample_cache_terminal_ready(&refused_terminal_cache),
              "refused final boundary leaves all six slots intact")) {
    return 1;
  }
  refused_terminal_cache.acquisition_overrun_latched[2] = true;
  if (!expect(!ikb_sample_cache_terminal_ready(&refused_terminal_cache),
              "latched cache fault blocks the final aggregate")) {
    return 1;
  }

  ikb_sample_cache_t boundary_cache;
  ikb_sample_cache_initialize(&boundary_cache);
  ikb_hal_sample_t boundary_input = {
      .sample_time_us = target_time_us,
      .fresh = true,
  };
  ikb_hal_sample_t boundary_snapshot[IKB_SENSOR_COUNT] = {0};
  cache_marker_trace_t marker_trace = {
      .cache = &boundary_cache,
      .samples = boundary_snapshot,
      .expected_sample_time_us = target_time_us,
      .sensor_pending = true,
  };
  const ikb_analyzer_marker_t trace_commit_marker_interface = {
      .context = &marker_trace,
      .set = trace_commit_marker,
      .delay_us = trace_commit_delay,
  };
  const ikb_analyzer_marker_t trace_snapshot_marker_interface = {
      .context = &marker_trace,
      .set = trace_snapshot_marker,
      .delay_us = trace_snapshot_delay,
  };
  if (!expect(ikb_sample_cache_commit(
                  &boundary_cache, 0U, &boundary_input,
                  &trace_commit_marker_interface, trace_sensor_ready,
                  &marker_trace) &&
                  ikb_sample_cache_snapshot(
                      &boundary_cache, boundary_snapshot,
                      &trace_snapshot_marker_interface),
              "cache commit becomes eligible before the aggregate snapshot") ||
      !expect(marker_trace.event_count == 7U &&
                  marker_trace.events[0] == COMMIT_MARKER_RISE &&
                  marker_trace.events[1] == COMMIT_MARKER_DELAY &&
                  marker_trace.events[2] == COMMIT_MARKER_FALL &&
                  marker_trace.events[3] == SENSOR_READY &&
                  marker_trace.events[4] == SNAPSHOT_MARKER_RISE &&
                  marker_trace.events[5] == SNAPSHOT_MARKER_DELAY &&
                  marker_trace.events[6] == SNAPSHOT_MARKER_FALL &&
                  marker_trace.commit_rise_preceded_store &&
                  marker_trace.commit_delay_followed_store &&
                  marker_trace.commit_fall_followed_store &&
                  marker_trace.ready_followed_commit_fall &&
                  marker_trace.snapshot_rise_followed_ready &&
                  marker_trace.snapshot_delay_followed_copy &&
                  marker_trace.snapshot_fall_followed_copy,
              "D9 fall publishes readiness before D8 cache consumption")) {
    return 1;
  }

  ikb_sample_cache_t commit_assertion_failure_cache;
  ikb_sample_cache_initialize(&commit_assertion_failure_cache);
  ikb_hal_sample_t commit_failure_snapshot[IKB_SENSOR_COUNT] = {0};
  failing_marker_t commit_assertion_failure = {
      .failing_call = 0U,
  };
  const ikb_analyzer_marker_t commit_assertion_failure_interface =
      failing_marker_interface(&commit_assertion_failure);
  test_marker_t commit_assertion_ready = {0};
  if (!expect(!ikb_sample_cache_commit(
                  &commit_assertion_failure_cache, 0U, &boundary_input,
                  &commit_assertion_failure_interface, publish_test_ready,
                  &commit_assertion_ready),
              "failed D9 assertion rejects the cache commit") ||
      !expect(commit_assertion_failure.calls == 1U &&
                  commit_assertion_failure.states[0] &&
                  commit_assertion_failure.delay_calls == 0U &&
                  commit_assertion_ready.ready &&
                  !commit_assertion_failure_cache.pending[0] &&
                  commit_assertion_failure_cache.latest[0].sample_time_us ==
                      0U,
              "failed D9 assertion stores nothing and still publishes readiness")) {
    return 1;
  }
  ikb_sample_cache_note_acquisition_fault(&commit_assertion_failure_cache,
                                          0U);
  if (!expect(snapshot_for_test(&commit_assertion_failure_cache,
                                commit_failure_snapshot) &&
                  !commit_failure_snapshot[0].fresh &&
                  commit_failure_snapshot[0].acquisition_overrun,
              "caller preserves failed D9 assertion as acquisition evidence")) {
    return 1;
  }

  ikb_sample_cache_t commit_deassertion_failure_cache;
  ikb_sample_cache_initialize(&commit_deassertion_failure_cache);
  failing_marker_t commit_deassertion_failure = {
      .failing_call = 1U,
  };
  const ikb_analyzer_marker_t commit_deassertion_failure_interface =
      failing_marker_interface(&commit_deassertion_failure);
  test_marker_t commit_deassertion_ready = {0};
  if (!expect(!ikb_sample_cache_commit(
                  &commit_deassertion_failure_cache, 0U, &boundary_input,
                  &commit_deassertion_failure_interface, publish_test_ready,
                  &commit_deassertion_ready),
              "failed D9 deassertion rejects the marked commit") ||
      !expect(commit_deassertion_failure.calls == 2U &&
                  commit_deassertion_failure.states[0] &&
                  !commit_deassertion_failure.states[1] &&
                  commit_deassertion_failure.delay_calls == 1U &&
                  commit_deassertion_failure.delayed_us ==
                      IKB_ANALYZER_MARKER_MIN_HIGH_US &&
                  commit_deassertion_ready.ready &&
                  commit_deassertion_failure_cache.pending[0] &&
                  commit_deassertion_failure_cache.latest[0].sample_time_us ==
                      target_time_us,
              "failed D9 deassertion returns false after delay, store, and readiness")) {
    return 1;
  }
  ikb_sample_cache_note_acquisition_fault(&commit_deassertion_failure_cache,
                                          0U);
  if (!expect(snapshot_for_test(&commit_deassertion_failure_cache,
                                commit_failure_snapshot) &&
                  commit_failure_snapshot[0].fresh &&
                  commit_failure_snapshot[0].sample_time_us ==
                      target_time_us &&
                  commit_failure_snapshot[0].acquisition_overrun,
              "caller faults the stored sample after D9 deassertion failure")) {
    return 1;
  }

  ikb_sample_cache_t marker_failure_cache;
  ikb_sample_cache_initialize(&marker_failure_cache);
  ikb_hal_sample_t marker_failure_snapshot[IKB_SENSOR_COUNT] = {0};
  failing_marker_t marker_assertion_failure = {
      .failing_call = 0U,
  };
  const ikb_analyzer_marker_t marker_assertion_failure_interface =
      failing_marker_interface(&marker_assertion_failure);
  if (!expect(commit_for_test(&marker_failure_cache, 0U,
                              &boundary_input) &&
                  !ikb_sample_cache_snapshot(
                      &marker_failure_cache, marker_failure_snapshot,
                      &marker_assertion_failure_interface),
              "failed marker assertion rejects the snapshot") ||
      !expect(marker_assertion_failure.calls == 2U &&
                  marker_assertion_failure.states[0] &&
                  !marker_assertion_failure.states[1] &&
                  marker_assertion_failure.delay_calls == 0U &&
                  marker_failure_cache.pending[0],
              "failed assertion attempts low cleanup without consuming data")) {
    return 1;
  }

  failing_marker_t marker_deassertion_failure = {
      .failing_call = 1U,
  };
  const ikb_analyzer_marker_t marker_deassertion_failure_interface =
      failing_marker_interface(&marker_deassertion_failure);
  if (!expect(!ikb_sample_cache_snapshot(
                  &marker_failure_cache, marker_failure_snapshot,
                  &marker_deassertion_failure_interface),
              "failed marker deassertion rejects the completed snapshot") ||
      !expect(marker_deassertion_failure.calls == 2U &&
                  marker_deassertion_failure.states[0] &&
                  !marker_deassertion_failure.states[1] &&
                  marker_deassertion_failure.delay_calls == 1U &&
                  marker_deassertion_failure.delayed_us ==
                      IKB_ANALYZER_MARKER_MIN_HIGH_US &&
                  !marker_failure_cache.pending[0],
              "deassertion failure remains terminal after cache consumption")) {
    return 1;
  }

  /*
   * Two 208 Hz conversions can land between consecutive 200 Hz aggregates.
   * The bounded cache emits only the latest one. Its exact data-ready time
   * exposes the skipped conversion without classifying decimation as a fault.
   */
  ikb_sample_cache_t decimation_cache;
  ikb_sample_cache_initialize(&decimation_cache);
  ikb_hal_sample_t decimated_sample = {
      .sample_time_us = target_time_us - 9616U,
      .fresh = true,
  };
  decimated_sample.reading.acceleration[0] = 1;
  ikb_hal_sample_t decimation_snapshot[IKB_SENSOR_COUNT];
  if (!expect(commit_for_test(&decimation_cache, 0U,
                              &decimated_sample) &&
                  snapshot_for_test(&decimation_cache,
                                    decimation_snapshot),
              "first decimator sample is snapshotted") ||
      !expect(decimation_snapshot[0].sample_time_us ==
                  target_time_us - 9616U,
              "first data-ready edge is retained")) {
    return 1;
  }
  decimated_sample.sample_time_us = target_time_us - 4808U;
  decimated_sample.reading.acceleration[0] = 2;
  if (!expect(commit_for_test(&decimation_cache, 0U,
                              &decimated_sample),
              "intermediate conversion enters latest-sample cache")) {
    return 1;
  }
  decimated_sample.sample_time_us = target_time_us;
  decimated_sample.reading.acceleration[0] = 3;
  if (!expect(commit_for_test(&decimation_cache, 0U,
                              &decimated_sample) &&
                  snapshot_for_test(&decimation_cache,
                                    decimation_snapshot),
              "newest conversion replaces the unread intermediate sample") ||
      !expect(decimation_snapshot[0].fresh &&
                  !decimation_snapshot[0].acquisition_overrun &&
                  decimation_snapshot[0].sample_time_us == target_time_us &&
                  decimation_snapshot[0].reading.acceleration[0] == 3,
              "normal decimation keeps the latest edge without a fault")) {
    return 1;
  }
  decimated_sample.sample_time_us = target_time_us + 4808U;
  decimated_sample.reading.acceleration[0] = 4;
  if (!expect(commit_for_test(&decimation_cache, 0U,
                              &decimated_sample),
              "post-snapshot conversion enters the next aggregate") ||
      !expect(decimation_snapshot[0].sample_time_us == target_time_us &&
                  decimation_snapshot[0].reading.acceleration[0] == 3,
              "completed snapshot is immutable after the boundary") ||
      !expect(snapshot_for_test(&decimation_cache,
                                decimation_snapshot) &&
                  decimation_snapshot[0].sample_time_us ==
                      target_time_us + 4808U &&
                  decimation_snapshot[0].reading.acceleration[0] == 4,
              "post-boundary conversion enters only the next snapshot")) {
    return 1;
  }

  ikb_sample_cache_t fault_cache;
  ikb_sample_cache_initialize(&fault_cache);
  ikb_hal_sample_t faulted_sample = {
      .sample_time_us = target_time_us - 4808U,
      .acquisition_overrun = true,
  };
  ikb_hal_sample_t recovered_sample = {
      .sample_time_us = target_time_us,
      .fresh = true,
  };
  recovered_sample.reading.acceleration[0] = 9;
  if (!expect(commit_for_test(&fault_cache, 0U, &faulted_sample) &&
                  commit_for_test(&fault_cache, 0U,
                                  &recovered_sample) &&
                  snapshot_for_test(&fault_cache,
                                    decimation_snapshot),
              "clean conversion can replace a faulted cache value") ||
      !expect(decimation_snapshot[0].fresh &&
                  decimation_snapshot[0].reading.acceleration[0] == 9 &&
                  decimation_snapshot[0].acquisition_overrun,
              "recovery preserves the preceding real acquisition fault")) {
    return 1;
  }

  fake_bus.status_delay_reads[3] = 2U;
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    ikb_hal_sample_t sample;
    if (!expect(ikb_sensor_bank_read(&bus, sensor,
                                     target_time_us - sensor * 10U, &sample),
                "data-ready sample reads through its mux channel") ||
        !expect(sample.reading.angular_velocity[0] == 100 + sensor &&
                    sample.reading.acceleration[2] == -300 - sensor,
                "sensor register order maps to protocol axes") ||
        !expect(commit_for_test(&cache, sensor, &sample),
                "sample enters bounded cache")) {
      return 1;
    }
  }
  if (!expect(fake_bus.delayed_us == 20U,
              "a bounded status poll waits for a coherent gyro/accel burst")) {
    return 1;
  }
  if (!expect(!fake_bus.unexpected_register_read,
              "sensor acquisition reads only configured status and output registers")) {
    return 1;
  }

  fake_bus.status_delay_reads[4] = 20U;
  ikb_hal_sample_t timeout_sample;
  if (!expect(ikb_sensor_bank_read(&bus, 4U, target_time_us,
                                   &timeout_sample),
              "data-ready timeout remains readable fault evidence") ||
      !expect(!timeout_sample.fresh && timeout_sample.acquisition_overrun,
              "data-ready timeout is never emitted as a mixed-age sample") ||
      !expect(fake_bus.delayed_us == 210U,
              "data-ready coherence wait is bounded to 190 us")) {
    return 1;
  }
  ikb_sample_cache_t timeout_priming_cache;
  ikb_sample_cache_initialize(&timeout_priming_cache);
  if (!expect(commit_for_test(&timeout_priming_cache, 4U, &timeout_sample) &&
                  ikb_sample_cache_acquisition_faulted(&timeout_priming_cache) &&
                  !ikb_sample_cache_terminal_ready(&timeout_priming_cache),
              "real status-poll timeout fails priming instead of waiting")) {
    return 1;
  }

  ikb_hal_sample_t replacement;
  if (!expect(ikb_sensor_bank_read(&bus, 0U, target_time_us, &replacement),
              "replacement sample reads") ||
      !expect(commit_for_test(&cache, 0U, &replacement),
              "unread replacement is retained as normal decimation")) {
    return 1;
  }
  ikb_sample_cache_note_acquisition_fault(&cache, 2U);

  const uint8_t controller_id[IKB_CONTROLLER_ID_SHA256_LEN] = {[0] = 0x41U};
  const uint8_t boot_id[IKB_BOOT_ID_LEN] = {[0] = 0x42U};
  ikb_acquisition_state_t acquisition;
  aggregate_context_t aggregate_context = {
      .now_us = target_time_us + 200U,
  };
  const ikb_acquisition_hal_t acquisition_hal = {
      .context = &aggregate_context,
      .read_latest = aggregate_take,
      .now_us = aggregate_now_us,
  };
  if (!expect(ikb_acquisition_init(&acquisition, IKB_HAND_LEFT, controller_id,
                                   boot_id),
              "portable acquisition accepts generated identities")) {
    return 1;
  }

  if (!expect(snapshot_for_test(&cache, aggregate_context.samples),
              "all six cache slots cross one aggregate boundary") ||
      !expect(aggregate_context.samples[0].fresh &&
                  !aggregate_context.samples[0].acquisition_overrun &&
                  aggregate_context.samples[0].sample_time_us == target_time_us,
              "normal latest-sample replacement remains nonfaulting") ||
      !expect(aggregate_context.samples[2].acquisition_overrun,
              "pending-read collision remains an explicit fault")) {
    return 1;
  }
  ikb_sensor_frame_t acquired_frame;
  if (!expect(ikb_acquire_frame(&acquisition, &acquisition_hal,
                                target_time_us,
                                target_time_us + IKB_AGGREGATE_PERIOD_US,
                                &acquired_frame),
              "fake HAL feeds the exact portable acquisition boundary") ||
      !expect(acquired_frame.sequence == 0U,
              "first normal D8 snapshot maps to frame sequence zero") ||
      !expect(acquired_frame.valid_sensor_mask == IKB_ALL_SENSORS_VALID,
              "all cached sensor channels enter one aggregate") ||
      !expect((acquired_frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "pending-read collision evidence reaches the aggregate") ||
      !expect(acquired_frame.sensors[0].sample_time_offset_us == 0,
              "latest replacement data-ready time reaches the wire sample") ||
      !expect(acquired_frame.sensors[5].sample_time_offset_us == -50,
              "cached data-ready timestamp reaches the wire sample")) {
    return 1;
  }

  aggregate_context.now_us += IKB_AGGREGATE_PERIOD_US;
  if (!expect(snapshot_for_test(&cache, aggregate_context.samples),
              "next aggregate snapshots the now-empty cache") ||
      !expect(!aggregate_context.samples[2].fresh &&
                  !aggregate_context.samples[2].acquisition_overrun,
              "reported pending-read fault clears after one snapshot") ||
      !expect(ikb_acquire_frame(&acquisition, &acquisition_hal,
                                target_time_us + IKB_AGGREGATE_PERIOD_US,
                                target_time_us +
                                    2U * IKB_AGGREGATE_PERIOD_US,
                                &acquired_frame),
              "empty cache still emits fault evidence") ||
      !expect(acquired_frame.sequence == 1U,
              "each later D8 snapshot advances sequence exactly once") ||
      !expect(acquired_frame.valid_sensor_mask == 0U,
              "consumed samples are not repeated as fresh") ||
      !expect((acquired_frame.flags & IKB_FLAG_ACQUISITION_OVERRUN) != 0U,
              "missing fresh channels mark aggregate overrun")) {
    return 1;
  }

  uint8_t derived_id[IKB_CONTROLLER_ID_SHA256_LEN];
  static const uint8_t provisioned_controller_id[] =
      "invisible-keyboard-left-001";
  if (!expect(ikb_derive_controller_id(provisioned_controller_id,
                                       sizeof(provisioned_controller_id) - 1U,
                                       derived_id),
              "controller ID hashes the provisioned manifest string") ||
      !expect(expect_hex(
                  derived_id, sizeof(derived_id),
                  "c023c2b4bafb37077fce4abaecb00bf876ed809f8fab5522e2714fc08620b504"),
              "controller ID matches host manifest SHA-256")) {
    return 1;
  }

  ikb_transport_slot_t transport_slot;
  ikb_transport_slot_initialize(&transport_slot);
  uint8_t queued_frame[IKB_TRANSPORT_MAX_LEN];
  memset(queued_frame, 0x5a, sizeof(queued_frame));
  if (!expect(ikb_transport_slot_offer(&transport_slot, queued_frame,
                                       sizeof(queued_frame)),
              "bounded USB slot accepts one complete frame") ||
      !expect(!ikb_transport_slot_offer(&transport_slot, queued_frame,
                                        sizeof(queued_frame)),
              "bounded USB slot rejects an overlapping frame")) {
    return 1;
  }
  const uint8_t *queued_bytes = NULL;
  if (!expect(ikb_transport_slot_peek(&transport_slot, &queued_bytes) == 162U &&
                  queued_bytes[0] == 0x5aU,
              "USB slot exposes its pending frame") ||
      !expect(ikb_transport_slot_consume(&transport_slot, 64U),
              "USB slot consumes a partial driver write") ||
      !expect(ikb_transport_slot_peek(&transport_slot, &queued_bytes) == 98U &&
                  queued_bytes == &transport_slot.bytes[64],
              "USB slot retains only the unsent suffix") ||
      !expect(ikb_transport_slot_consume(&transport_slot, 98U),
              "USB slot completes the frame") ||
      !expect(ikb_transport_slot_peek(&transport_slot, &queued_bytes) == 0U &&
                  queued_bytes == NULL,
              "completed USB slot becomes available") ||
      !expect(!ikb_transport_slot_offer(&transport_slot, queued_frame, 0U),
              "USB slot rejects an empty write") ||
      !expect(!ikb_transport_slot_offer(&transport_slot, queued_frame,
                                        sizeof(queued_frame) + 1U),
              "USB slot rejects a frame beyond its fixed bound")) {
    return 1;
  }

  fake_entropy_t entropy = {.next_value = 1U};
  uint8_t generated_boot_id[IKB_BOOT_ID_LEN];
  if (!expect(ikb_create_boot_id(&entropy, fake_random_fill,
                                 generated_boot_id),
              "boot ID accepts nonzero CSPRNG bytes") ||
      !expect(generated_boot_id[0] == 1U && generated_boot_id[15] == 16U,
              "boot ID retains random bytes")) {
    return 1;
  }
  entropy.fail = true;
  if (!expect(!ikb_create_boot_id(&entropy, fake_random_fill,
                                  generated_boot_id),
              "entropy failure fails boot identity closed")) {
    return 1;
  }

  ikb_sensor_frame_t golden_frame = {0};
  golden_frame.hand = IKB_HAND_LEFT;
  memset(golden_frame.controller_id_sha256, 0x11,
         sizeof(golden_frame.controller_id_sha256));
  memset(golden_frame.boot_id, 0x22, sizeof(golden_frame.boot_id));
  golden_frame.valid_sensor_mask = IKB_ALL_SENSORS_VALID;
  uint8_t transport[IKB_TRANSPORT_MAX_LEN];
  const size_t transport_length = ikb_encode_transport_frame(
      &golden_frame, transport, sizeof(transport));
  if (!expect(transport_length == 162U, "golden COBS frame length") ||
      !expect(expect_hex(
                  transport, transport_length,
                  "06494b42320203063f01010101010101010101010101010131111111111111111111111111111111111111111111111111111111111111111122222222222222222222222222222222010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010105b6c63ee200"),
              "golden frame matches the host protocol fixture")) {
    return 1;
  }

  if (!expect(run_integrated_quiesce_path(controller_id, boot_id),
              "one production helper path reaches terminal silence")) {
    return 1;
  }

  return 0;
}
