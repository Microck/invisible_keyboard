#ifndef INVISIBLE_KEYBOARD_ZEPHYR_CONTROLLER_H_
#define INVISIBLE_KEYBOARD_ZEPHYR_CONTROLLER_H_

#include "firmware/acquisition.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define IKB_AGGREGATE_RATE_HZ 200U
#define IKB_AGGREGATE_PERIOD_US (1000000U / IKB_AGGREGATE_RATE_HZ)
#define IKB_SENSOR_ODR_HZ 208U
#define IKB_SENSOR_PRIMING_TIMEOUT_MS 100U
#define IKB_ANALYZER_MARKER_MIN_HIGH_US 2U
#define IKB_ANALYZER_STARTUP_PHASE_US 20U
#define IKB_ANALYZER_TERMINAL_IDLE_US 200U
#define IKB_ANALYZER_TERMINAL_MARKER_US 150U
#define IKB_SENSOR_SHUTDOWN_TRANSACTION_COUNT 52U

#define IKB_TCA9548A_ADDRESS 0x70U
#define IKB_LSM6DS3TR_C_ADDRESS 0x6aU
#define IKB_LSM6DS3TR_C_WHO_AM_I 0x6aU

typedef bool (*ikb_select_channel_fn)(void *context, uint8_t channel);
typedef bool (*ikb_deselect_all_fn)(void *context);
typedef bool (*ikb_write_register_fn)(void *context, uint8_t register_address,
                                      uint8_t value);
typedef bool (*ikb_read_register_fn)(void *context, uint8_t register_address,
                                     uint8_t *value);
typedef bool (*ikb_read_registers_fn)(void *context, uint8_t register_address,
                                     uint8_t *values, size_t length);
typedef void (*ikb_delay_us_fn)(void *context, uint32_t delay_us);

/* Serial bus boundary shared by Zephyr and the native transcript fixture. */
typedef struct {
  void *context;
  ikb_select_channel_fn select_channel;
  /* Shutdown-only operation. Normal acquisition remains one-hot. */
  ikb_deselect_all_fn deselect_all;
  ikb_write_register_fn write_register;
  ikb_read_register_fn read_register;
  ikb_read_registers_fn read_registers;
  ikb_delay_us_fn delay_us;
} ikb_sensor_bus_t;

/* Configure/read back all six devices, leaving INT1 disabled on every branch. */
bool ikb_sensor_bank_initialize(const ikb_sensor_bus_t *bus);

/*
 * Enable/read back INT1 after all GPIO callbacks have been installed. Run this
 * on the acquisition work queue so admitted sensor work cannot interleave mux
 * access with the finite arming operation.
 */
bool ikb_sensor_bank_arm(const ikb_sensor_bus_t *bus);

/* Read one latest gyro/accelerometer sample on an already initialized bank. */
bool ikb_sensor_bank_read(const ikb_sensor_bus_t *bus, uint8_t sensor_index,
                          uint64_t sample_time_us,
                          ikb_hal_sample_t *sample);

/*
 * Emit the fixed 52-transaction shutdown suffix. Every mux selection and
 * power-down register write is read back before the next transaction. The
 * operation stops at the first failure and never resumes acquisition.
 */
bool ikb_sensor_bank_shutdown(const ikb_sensor_bus_t *bus);

/*
 * One-slot-per-sensor latest-sample cache. The Zephyr adapter supplies
 * synchronization. Replacing an unread sample is the intentional 208 Hz to
 * 200 Hz decimator. Only an actual acquisition fault is latched until the next
 * aggregate consumes it.
 */
typedef struct {
  ikb_hal_sample_t latest[IKB_SENSOR_COUNT];
  bool pending[IKB_SENSOR_COUNT];
  bool acquisition_overrun_latched[IKB_SENSOR_COUNT];
} ikb_sample_cache_t;

typedef bool (*ikb_analyzer_marker_set_fn)(void *context, bool asserted);
typedef void (*ikb_sample_ready_fn)(void *context);
typedef bool (*ikb_close_admission_fn)(void *context);

typedef struct {
  void *context;
  ikb_analyzer_marker_set_fn set;
  ikb_delay_us_fn delay_us;
} ikb_analyzer_marker_t;

/*
 * Emit the unique capture prefix before sensor I/O or interrupts start:
 * D8 high, idle, D9 high, idle. Each phase requests the configured minimum
 * delay. Capture must begin before this prefix.
 */
bool ikb_analyzer_emit_startup_anchor(
    const ikb_analyzer_marker_t *snapshot_marker,
    const ikb_analyzer_marker_t *commit_marker);

void ikb_sample_cache_initialize(ikb_sample_cache_t *cache);
void ikb_sample_cache_note_acquisition_fault(ikb_sample_cache_t *cache,
                                             uint8_t sensor_index);

/*
 * Commit one I2C-complete sample under synchronization supplied by the
 * caller. The mandatory marker rises before the store, stays high for the
 * minimum analyzer interval, and falls afterward. publish_ready runs after
 * the falling-edge attempt while the caller still holds its readiness lock.
 * Failed assertion leaves the cache untouched but still publishes readiness.
 * Failed deassertion returns false after the store and readiness publication,
 * so the caller can preserve both data and fault evidence.
 */
bool ikb_sample_cache_commit(
    ikb_sample_cache_t *cache, uint8_t sensor_index,
    const ikb_hal_sample_t *sample,
    const ikb_analyzer_marker_t *commit_marker,
    ikb_sample_ready_fn publish_ready, void *ready_context);

/*
 * Atomically consume the six cache slots under synchronization supplied by
 * the caller. This call is the aggregate snapshot boundary: conversions
 * stored after it belong to the next 200 Hz aggregate. The mandatory marker
 * rises before the first cache read, stays high for the minimum analyzer
 * interval after the final cache clear, and then falls.
 */
bool ikb_sample_cache_snapshot(
    ikb_sample_cache_t *cache,
    ikb_hal_sample_t samples[IKB_SENSOR_COUNT],
    const ikb_analyzer_marker_t *snapshot_marker);

/* True only when a final aggregate can consume six fresh, clean samples. */
bool ikb_sample_cache_terminal_ready(const ikb_sample_cache_t *cache);

/* True for a latched acquisition fault or any occupied unclean cache slot. */
bool ikb_sample_cache_acquisition_faulted(const ikb_sample_cache_t *cache);

/*
 * Raise ordinary D8, close admission, and consume the final cache under the
 * caller's policy/cache/sensor lock set. Admission therefore closes after
 * the cutoff edge and before the first cache byte is copied.
 */
bool ikb_sample_cache_terminal_snapshot(
    ikb_sample_cache_t *cache,
    ikb_hal_sample_t samples[IKB_SENSOR_COUNT],
    const ikb_analyzer_marker_t *snapshot_marker,
    ikb_close_admission_fn close_admission, void *close_context);

typedef bool (*ikb_terminal_endpoints_idle_fn)(void *context);

typedef struct {
  void *context;
  /* The board adapter must change D8 and D9 in one port operation. */
  ikb_analyzer_marker_set_fn set_both;
  ikb_terminal_endpoints_idle_fn endpoints_idle;
  ikb_delay_us_fn delay_us;
} ikb_terminal_marker_t;

/*
 * Require 200 us of bus/INT1 idle, emit a nominal 150 us dual-pin marker,
 * and recheck the endpoints before the simultaneous falling edge.
 */
bool ikb_analyzer_emit_terminal_marker(
    const ikb_terminal_marker_t *terminal_marker);

/*
 * One bounded transport-record slot. The glue policy composes three of these
 * into the ordered normal/final/receipt queue. The adapter feeds the selected
 * CDC FIFO only from its interrupt callback and checks every write count.
 */
typedef struct {
  uint8_t bytes[IKB_TRANSPORT_MAX_LEN];
  size_t length;
  size_t offset;
  bool pending;
} ikb_transport_slot_t;

void ikb_transport_slot_initialize(ikb_transport_slot_t *slot);
bool ikb_transport_slot_offer(ikb_transport_slot_t *slot,
                              const uint8_t *bytes, size_t length);
size_t ikb_transport_slot_peek(const ikb_transport_slot_t *slot,
                               const uint8_t **bytes);
bool ikb_transport_slot_consume(ikb_transport_slot_t *slot, size_t length);
void ikb_transport_slot_drop(ikb_transport_slot_t *slot);

#endif
