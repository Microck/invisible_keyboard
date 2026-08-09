#include "controller.h"
#include "glue-policy.h"
#include "identity.h"

#include "firmware/acquisition.h"
#include "firmware/frame-protocol.h"

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/gpio.h>
#include <zephyr/drivers/i2c.h>
#include <zephyr/drivers/uart.h>
#include <zephyr/kernel.h>
#include <zephyr/random/random.h>
#include <zephyr/sys/util.h>

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

BUILD_ASSERT(IKB_SENSOR_COUNT == 6U, "overlay and protocol sensor count differ");
BUILD_ASSERT(IKB_SENSOR_ODR_HZ == 208U,
             "each IMU must retain the canonical 208 Hz profile");
BUILD_ASSERT(IKB_AGGREGATE_PERIOD_US == 5000U,
             "the independent aggregate cadence must remain at 200 Hz");
BUILD_ASSERT(IS_ENABLED(CONFIG_TIMEOUT_64BIT),
             "aggregate cadence requires absolute kernel timeouts");
BUILD_ASSERT(IS_ENABLED(CONFIG_CSPRNG_ENABLED),
             "the selected board must provide cryptographic boot entropy");
BUILD_ASSERT(sizeof(CONFIG_IKB_CONTROLLER_ID) > 1U,
             "a manifest controller_id must be provisioned into each image");
BUILD_ASSERT(
    DT_SAME_NODE(DT_GPIO_CTLR(DT_ALIAS(ikb_snapshot), gpios),
                 DT_GPIO_CTLR(DT_ALIAS(ikb_cache_commit), gpios)),
    "D8 and D9 must share one GPIO controller for the terminal marker");

enum {
  SENSOR_WORK_QUEUE_PRIORITY = 0,
  SENSOR_WORK_QUEUE_STACK_SIZE = 1536,
  SENSOR_STARTUP_DELAY_MS = 20,
  TCA9548A_SETTLE_US = 10,
  UART_RX_CHUNK_LEN = 32,
};

typedef struct {
  struct k_work work;
  struct gpio_callback gpio_callback;
  struct gpio_dt_spec drdy;
  ikb_glue_sensor_policy_t admission;
  uint8_t index;
} sensor_context_t;

typedef struct {
  ikb_hal_sample_t samples[IKB_SENSOR_COUNT];
} aggregate_snapshot_t;

typedef struct {
  uint8_t encoded[IKB_QUIESCE_REQUEST_ENCODED_LEN];
  size_t encoded_length;
  bool closed;
} quiesce_rx_t;

typedef enum {
  FINAL_BOUNDARY_NOT_SELECTED = 0,
  FINAL_BOUNDARY_CLOSED = 1,
  FINAL_BOUNDARY_FAILED = 2,
} final_boundary_result_t;

#define IKB_I2C_NODE DT_NODELABEL(i2c1)

static const struct device *const i2c_device = DEVICE_DT_GET(IKB_I2C_NODE);
static const struct device *const cdc_device =
    DEVICE_DT_GET_ONE(zephyr_cdc_acm_uart);
static const struct gpio_dt_spec snapshot_marker =
    GPIO_DT_SPEC_GET(DT_ALIAS(ikb_snapshot), gpios);
static const struct gpio_dt_spec cache_commit_marker =
    GPIO_DT_SPEC_GET(DT_ALIAS(ikb_cache_commit), gpios);
static const struct device *const gpio0 = DEVICE_DT_GET(DT_NODELABEL(gpio0));

/* The XIAO BLE routes its I2C pads directly to P0.04 and P0.05. Reading the
 * GPIO input register does not reconfigure or take ownership from TWIM; it is
 * the least invasive way to prove both bus endpoints are high before D8+D9.
 */
#define IKB_BUS_SDA_PIN 4U
#define IKB_BUS_SCL_PIN 5U

static sensor_context_t sensors[IKB_SENSOR_COUNT] = {
    {.drdy = GPIO_DT_SPEC_GET(DT_ALIAS(ikb_drdy0), gpios)},
    {.drdy = GPIO_DT_SPEC_GET(DT_ALIAS(ikb_drdy1), gpios)},
    {.drdy = GPIO_DT_SPEC_GET(DT_ALIAS(ikb_drdy2), gpios)},
    {.drdy = GPIO_DT_SPEC_GET(DT_ALIAS(ikb_drdy3), gpios)},
    {.drdy = GPIO_DT_SPEC_GET(DT_ALIAS(ikb_drdy4), gpios)},
    {.drdy = GPIO_DT_SPEC_GET(DT_ALIAS(ikb_drdy5), gpios)},
};

K_THREAD_STACK_DEFINE(sensor_work_queue_stack, SENSOR_WORK_QUEUE_STACK_SIZE);
static struct k_work_q sensor_work_queue;
static struct k_work sensor_bank_arm_work;
static bool sensor_bank_arm_ok;
K_SEM_DEFINE(sensor_bank_arm_sem, 0, 1);
static bool sensor_priming_active = true;
static uint64_t sensor_priming_deadline_us;
K_SEM_DEFINE(sensor_priming_sem, 0, 1);
static struct k_spinlock sensor_state_lock;
static struct k_spinlock sample_cache_lock;
static struct k_spinlock glue_policy_lock;
static ikb_sample_cache_t sample_cache;
static ikb_glue_policy_t glue_policy;
static quiesce_rx_t quiesce_rx;
static ikb_hand_t controller_hand;
static uint8_t controller_id[IKB_CONTROLLER_ID_SHA256_LEN];
static uint8_t controller_boot_id[IKB_BOOT_ID_LEN];
K_SEM_DEFINE(aggregate_sem, 0, 1);
static struct k_timer aggregate_timer;
static uint64_t aggregate_timer_target_time_us;
static bool aggregate_timer_active;

static uint64_t now_us(void) {
  return k_ticks_to_us_floor64(k_uptime_ticks());
}

static bool set_mux_mask(void *context, uint8_t mask) {
  const struct device *device = context;
  if (i2c_write(device, &mask, sizeof(mask), IKB_TCA9548A_ADDRESS) != 0) {
    return false;
  }
  k_busy_wait(TCA9548A_SETTLE_US);
  uint8_t readback = 0U;
  return i2c_read(device, &readback, sizeof(readback),
                  IKB_TCA9548A_ADDRESS) == 0 &&
         readback == mask;
}

static bool select_channel(void *context, uint8_t channel) {
  return channel < IKB_SENSOR_COUNT &&
         set_mux_mask(context, (uint8_t)BIT(channel));
}

static bool deselect_all(void *context) {
  return set_mux_mask(context, 0U);
}

static bool write_register(void *context, uint8_t register_address,
                           uint8_t value) {
  return i2c_reg_write_byte(context, IKB_LSM6DS3TR_C_ADDRESS,
                            register_address, value) == 0;
}

static bool read_register(void *context, uint8_t register_address,
                          uint8_t *value) {
  return i2c_reg_read_byte(context, IKB_LSM6DS3TR_C_ADDRESS,
                           register_address, value) == 0;
}

static bool read_registers(void *context, uint8_t register_address,
                           uint8_t *values, size_t length) {
  return i2c_burst_read(context, IKB_LSM6DS3TR_C_ADDRESS, register_address,
                        values, length) == 0;
}

static void delay_us(void *context, uint32_t delay) {
  ARG_UNUSED(context);
  k_busy_wait(delay);
}

static const ikb_sensor_bus_t sensor_bus = {
    .context = (void *)DEVICE_DT_GET(IKB_I2C_NODE),
    .select_channel = select_channel,
    .deselect_all = deselect_all,
    .write_register = write_register,
    .read_register = read_register,
    .read_registers = read_registers,
    .delay_us = delay_us,
};

static bool set_analyzer_marker(void *context, bool asserted) {
  const struct gpio_dt_spec *marker = context;
  return marker != NULL && gpio_pin_set_dt(marker, asserted ? 1 : 0) == 0;
}

static const ikb_analyzer_marker_t snapshot_analyzer_marker = {
    .context = (void *)&snapshot_marker,
    .set = set_analyzer_marker,
    .delay_us = delay_us,
};
static const ikb_analyzer_marker_t cache_commit_analyzer_marker = {
    .context = (void *)&cache_commit_marker,
    .set = set_analyzer_marker,
    .delay_us = delay_us,
};

static void latch_sensor_acquisition_fault(uint8_t sensor_index) {
  k_spinlock_key_t glue_key = k_spin_lock(&glue_policy_lock);
  k_spinlock_key_t cache_key = k_spin_lock(&sample_cache_lock);
  ikb_sample_cache_note_acquisition_fault(&sample_cache, sensor_index);
  const bool wake_priming = sensor_priming_active;
  k_spin_unlock(&sample_cache_lock, cache_key);
  k_spin_unlock(&glue_policy_lock, glue_key);
  if (wake_priming) {
    k_sem_give(&sensor_priming_sem);
  }
}

static bool prime_sensor_cache(void) {
  for (;;) {
    k_spinlock_key_t glue_key = k_spin_lock(&glue_policy_lock);
    k_spinlock_key_t cache_key = k_spin_lock(&sample_cache_lock);
    const bool faulted = ikb_glue_policy_failed(&glue_policy) ||
                         ikb_sample_cache_acquisition_faulted(&sample_cache);
    const bool ready =
        !faulted && ikb_sample_cache_terminal_ready(&sample_cache);
    const uint64_t current_time_us = now_us();
    const bool finished =
        faulted || ready || current_time_us >= sensor_priming_deadline_us;
    if (finished) {
      sensor_priming_active = false;
    }
    k_spin_unlock(&sample_cache_lock, cache_key);
    k_spin_unlock(&glue_policy_lock, glue_key);
    if (finished) {
      return !faulted && ready &&
             current_time_us <= sensor_priming_deadline_us;
    }
    /* Release every cache/policy lock before waiting for ordinary worker D9. */
    (void)k_sem_take(&sensor_priming_sem,
                     K_USEC(sensor_priming_deadline_us - current_time_us));
  }
}

static void publish_sensor_ready(void *context) {
  sensor_context_t *sensor = context;
  (void)ikb_glue_sensor_complete(&sensor->admission);
}

/* IRQs may enqueue sensor work while arming, but this same-queue work item
 * keeps their mux accesses behind every enable write and readback.
 */
static void sensor_bank_arm_work_handler(struct k_work *work) {
  ARG_UNUSED(work);
  sensor_bank_arm_ok = ikb_sensor_bank_arm(&sensor_bus);
  /* Main's scheduling delay must not restart the fixed priming window. */
  if (sensor_bank_arm_ok) {
    sensor_priming_deadline_us =
        now_us() + (uint64_t)IKB_SENSOR_PRIMING_TIMEOUT_MS * 1000U;
  }
  k_sem_give(&sensor_bank_arm_sem);
}

static void sensor_work_handler(struct k_work *work) {
  sensor_context_t *sensor = CONTAINER_OF(work, sensor_context_t, work);
  uint64_t sample_time_us = 0U;

  k_spinlock_key_t glue_key = k_spin_lock(&glue_policy_lock);
  k_spinlock_key_t state_key = k_spin_lock(&sensor_state_lock);
  const bool work_started =
      ikb_glue_sensor_begin(&sensor->admission, &sample_time_us);
  if (!work_started) {
    ikb_glue_policy_fail(&glue_policy);
  }
  k_spin_unlock(&sensor_state_lock, state_key);
  k_spin_unlock(&glue_policy_lock, glue_key);
  if (!work_started) {
    latch_sensor_acquisition_fault(sensor->index);
    return;
  }

  ikb_hal_sample_t sample;
  const bool read_ok = ikb_sensor_bank_read(&sensor_bus, sensor->index,
                                             sample_time_us, &sample);
  if (!read_ok) {
    memset(&sample, 0, sizeof(sample));
    sample.sample_time_us = sample_time_us;
    sample.acquisition_overrun = true;
  }

  /*
   * Keep the documented lock order through D9 fall and sensor completion.
   * A final-boundary predicate can never observe the cache without also
   * observing this sensor as RUNNING.
   */
  glue_key = k_spin_lock(&glue_policy_lock);
  k_spinlock_key_t cache_key = k_spin_lock(&sample_cache_lock);
  state_key = k_spin_lock(&sensor_state_lock);
  const bool commit_ok = ikb_sample_cache_commit(
      &sample_cache, sensor->index, &sample, &cache_commit_analyzer_marker,
      publish_sensor_ready, sensor);
  if (!commit_ok) {
    ikb_sample_cache_note_acquisition_fault(&sample_cache, sensor->index);
    ikb_glue_policy_fail(&glue_policy);
  }
  const bool wake_priming = sensor_priming_active;
  k_spin_unlock(&sensor_state_lock, state_key);
  k_spin_unlock(&sample_cache_lock, cache_key);
  k_spin_unlock(&glue_policy_lock, glue_key);
  if (wake_priming) {
    k_sem_give(&sensor_priming_sem);
  }
}

static void data_ready_handler(const struct device *port,
                               struct gpio_callback *callback,
                               gpio_port_pins_t pins) {
  ARG_UNUSED(port);
  ARG_UNUSED(pins);
  sensor_context_t *sensor =
      CONTAINER_OF(callback, sensor_context_t, gpio_callback);
  const uint64_t edge_time_us = now_us();
  bool acquisition_fault = false;

  k_spinlock_key_t glue_key = k_spin_lock(&glue_policy_lock);
  k_spinlock_key_t state_key = k_spin_lock(&sensor_state_lock);
  if (!ikb_glue_admission_open(&glue_policy)) {
    k_spin_unlock(&sensor_state_lock, state_key);
    k_spin_unlock(&glue_policy_lock, glue_key);
    return;
  }

  if (!ikb_glue_sensor_admit(&sensor->admission, edge_time_us)) {
    acquisition_fault = true;
  } else {
    const int submission =
        k_work_submit_to_queue(&sensor_work_queue, &sensor->work);
    /* Zephyr returns 2 when the prior handler is in its post-D9 tail and the
     * new instance was queued behind it. Both 1 and 2 own a future run.
     */
    if ((submission != 1 && submission != 2) ||
        !ikb_glue_sensor_mark_submitted(&sensor->admission)) {
      (void)ikb_glue_sensor_abort_submission(&sensor->admission);
      ikb_glue_policy_fail(&glue_policy);
      acquisition_fault = true;
    }
  }
  k_spin_unlock(&sensor_state_lock, state_key);
  k_spin_unlock(&glue_policy_lock, glue_key);

  if (acquisition_fault) {
    latch_sensor_acquisition_fault(sensor->index);
  }
}

static bool read_snapshot_sample(void *context, uint8_t sensor_index,
                                 ikb_hal_sample_t *sample) {
  const aggregate_snapshot_t *snapshot = context;
  if (snapshot == NULL || sensor_index >= IKB_SENSOR_COUNT || sample == NULL) {
    return false;
  }
  *sample = snapshot->samples[sensor_index];
  return true;
}

static uint64_t acquisition_now_us(void *context) {
  ARG_UNUSED(context);
  return now_us();
}

/* Caller holds glue_policy_lock, including across the timer rearm/stop. */
static void arm_next_aggregate(void) {
  const uint64_t tick = ikb_glue_next_aggregate_tick(
      &aggregate_timer_target_time_us, CONFIG_SYS_CLOCK_TICKS_PER_SEC);
  k_timer_start(&aggregate_timer, K_TIMEOUT_ABS_TICKS(tick), K_NO_WAIT);
}

static void stop_aggregate_timer(void) {
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  aggregate_timer_active = false;
  k_timer_stop(&aggregate_timer);
  k_spin_unlock(&glue_policy_lock, key);
}

static void aggregate_timer_handler(struct k_timer *timer) {
  ARG_UNUSED(timer);
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  bool wake_aggregate = false;
  if (aggregate_timer_active && ikb_glue_admission_open(&glue_policy)) {
    wake_aggregate = ikb_glue_timer_elapsed(&glue_policy);
    /* Zephyr replays expired absolute deadlines during a late announcement.
     * Retain each period instead of resetting the cadence to ISR uptime,
     * which itself reports the scheduled event time inside this callback.
     */
    arm_next_aggregate();
  }
  k_spin_unlock(&glue_policy_lock, key);
  if (wake_aggregate) {
    k_sem_give(&aggregate_sem);
  }
}

static void protocol_failure(void) {
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  ikb_glue_policy_fail(&glue_policy);
  k_spin_unlock(&glue_policy_lock, key);
}

static void process_quiesce_byte(uint8_t byte) {
  if (quiesce_rx.closed) {
    protocol_failure();
    return;
  }
  if (byte != 0U) {
    if (quiesce_rx.encoded_length >= sizeof(quiesce_rx.encoded)) {
      quiesce_rx.closed = true;
      protocol_failure();
      return;
    }
    quiesce_rx.encoded[quiesce_rx.encoded_length++] = byte;
    return;
  }

  quiesce_rx.closed = true;
  uint8_t decoded[IKB_QUIESCE_REQUEST_LEN];
  const size_t decoded_length = ikb_decode_transport_record(
      quiesce_rx.encoded, quiesce_rx.encoded_length, decoded,
      sizeof(decoded));
  ikb_quiesce_request_t request;
  if (quiesce_rx.encoded_length == 0U ||
      decoded_length != sizeof(decoded) ||
      !ikb_decode_quiesce_request(decoded, decoded_length, &request)) {
    protocol_failure();
    return;
  }

  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  (void)ikb_glue_request_quiesce(
      &glue_policy, &request, controller_hand, controller_id,
      controller_boot_id, now_us());
  k_spin_unlock(&glue_policy_lock, key);
}

static void drop_transport_record(const struct device *device) {
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  ikb_glue_transport_drop(&glue_policy);
  uart_irq_tx_disable(device);
  k_spin_unlock(&glue_policy_lock, key);
}

static void cdc_interrupt_handler(const struct device *device,
                                  void *user_data) {
  ARG_UNUSED(user_data);
  if (uart_irq_update(device) <= 0) {
    drop_transport_record(device);
    return;
  }

  const int uart_error = uart_err_check(device);
  if (uart_error != 0) {
    protocol_failure();
  }

  if (uart_irq_rx_ready(device) > 0) {
    uint8_t bytes[UART_RX_CHUNK_LEN];
    for (;;) {
      const int count = uart_fifo_read(device, bytes, sizeof(bytes));
      if (count <= 0) {
        if (count < 0) {
          protocol_failure();
        }
        break;
      }
      for (int index = 0; index < count; ++index) {
        process_quiesce_byte(bytes[index]);
      }
      if ((size_t)count < sizeof(bytes)) {
        break;
      }
    }
  }

  if (uart_irq_tx_ready(device) <= 0) {
    return;
  }
  const uint8_t *bytes = NULL;
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  const size_t remaining = ikb_glue_transport_peek(&glue_policy, &bytes);
  if (remaining == 0U) {
    uart_irq_tx_disable(device);
    k_spin_unlock(&glue_policy_lock, key);
    return;
  }
  k_spin_unlock(&glue_policy_lock, key);

  const int written = uart_fifo_fill(device, bytes, (int)remaining);
  key = k_spin_lock(&glue_policy_lock);
  const ikb_glue_transport_result_t write_result =
      ikb_glue_transport_written(&glue_policy, written);
  if (write_result == IKB_GLUE_TRANSPORT_DROPPED ||
      (write_result == IKB_GLUE_TRANSPORT_COMPLETE &&
       ikb_glue_transport_peek(&glue_policy, &bytes) == 0U)) {
    uart_irq_tx_disable(device);
  }
  k_spin_unlock(&glue_policy_lock, key);
}

static bool queue_normal_record(const uint8_t *bytes, size_t length) {
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  const bool queued = ikb_glue_transport_offer(&glue_policy, bytes, length);
  if (queued) {
    uart_irq_tx_enable(cdc_device);
  }
  k_spin_unlock(&glue_policy_lock, key);
  return queued;
}

static bool sensors_all_idle(void) {
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (ikb_glue_sensor_state(&sensors[sensor].admission) !=
        IKB_GLUE_SENSOR_IDLE) {
      return false;
    }
  }
  return true;
}

/* Called by the cache helper after D8 rises with all three locks held. */
static bool close_final_boundary(void *context) {
  ARG_UNUSED(context);
  if (!ikb_glue_close_boundary(&glue_policy)) {
    return false;
  }
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    if (!ikb_glue_sensor_close(&sensors[sensor].admission)) {
      ikb_glue_policy_fail(&glue_policy);
      return false;
    }
  }
  return true;
}

static final_boundary_result_t try_final_boundary(
    aggregate_snapshot_t *snapshot, uint64_t target_time_us) {
  k_spinlock_key_t glue_key = k_spin_lock(&glue_policy_lock);
  if (ikb_glue_policy_lifecycle(&glue_policy) !=
          IKB_GLUE_STOP_REQUESTED ||
      ikb_glue_policy_failed(&glue_policy)) {
    k_spin_unlock(&glue_policy_lock, glue_key);
    return FINAL_BOUNDARY_NOT_SELECTED;
  }
  k_spinlock_key_t cache_key = k_spin_lock(&sample_cache_lock);
  k_spinlock_key_t state_key = k_spin_lock(&sensor_state_lock);

  const uint64_t decision_time_us = now_us();
  const bool ready = ikb_glue_final_boundary_ready(
      &glue_policy, decision_time_us,
      ikb_sample_cache_terminal_ready(&sample_cache), sensors_all_idle(),
      decision_time_us <= target_time_us + IKB_AGGREGATE_PERIOD_US);
  bool snapshot_ok = false;
  bool boundary_closed = false;
  if (ready) {
    snapshot_ok = ikb_sample_cache_terminal_snapshot(
        &sample_cache, snapshot->samples, &snapshot_analyzer_marker,
        close_final_boundary, NULL);
    if (!snapshot_ok) {
      ikb_glue_policy_fail(&glue_policy);
    }
    boundary_closed =
        ikb_glue_policy_lifecycle(&glue_policy) >= IKB_GLUE_BOUNDARY_CLOSED;
  }

  k_spin_unlock(&sensor_state_lock, state_key);
  k_spin_unlock(&sample_cache_lock, cache_key);
  k_spin_unlock(&glue_policy_lock, glue_key);

  if (!ready) {
    return FINAL_BOUNDARY_NOT_SELECTED;
  }
  if (snapshot_ok && boundary_closed) {
    return FINAL_BOUNDARY_CLOSED;
  }
  return boundary_closed ? FINAL_BOUNDARY_FAILED
                         : FINAL_BOUNDARY_NOT_SELECTED;
}

static bool disable_sensor_interrupts(void) {
  bool disabled = true;
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT; ++sensor) {
    disabled &= gpio_pin_interrupt_configure_dt(&sensors[sensor].drdy,
                                                 GPIO_INT_DISABLE) == 0;
  }
  return disabled;
}

static gpio_port_pins_t terminal_marker_mask(void) {
  return (gpio_port_pins_t)(BIT(snapshot_marker.pin) |
                            BIT(cache_commit_marker.pin));
}

static bool force_terminal_marker_low(void) {
  return gpio_port_set_masked_raw(snapshot_marker.port,
                                  terminal_marker_mask(), 0U) == 0;
}

static bool set_terminal_marker(void *context, bool asserted) {
  ARG_UNUSED(context);
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  if (asserted && ikb_glue_policy_failed(&glue_policy)) {
    k_spin_unlock(&glue_policy_lock, key);
    return false;
  }
  const gpio_port_value_t value =
      asserted ? (gpio_port_value_t)terminal_marker_mask() : 0U;
  const bool changed =
      gpio_port_set_masked_raw(snapshot_marker.port, terminal_marker_mask(),
                               value) == 0;
  if (!changed) {
    ikb_glue_policy_fail(&glue_policy);
  }
  k_spin_unlock(&glue_policy_lock, key);
  return changed;
}

static bool terminal_endpoints_idle(void *context) {
  ARG_UNUSED(context);
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  bool idle = !ikb_glue_policy_failed(&glue_policy) &&
              gpio_pin_get_raw(gpio0, IKB_BUS_SDA_PIN) == 1 &&
              gpio_pin_get_raw(gpio0, IKB_BUS_SCL_PIN) == 1;
  for (uint8_t sensor = 0U; sensor < IKB_SENSOR_COUNT && idle; ++sensor) {
    idle = gpio_pin_get_dt(&sensors[sensor].drdy) == 0;
  }
  k_spin_unlock(&glue_policy_lock, key);
  return idle;
}

static const ikb_terminal_marker_t terminal_analyzer_marker = {
    .context = NULL,
    .set_both = set_terminal_marker,
    .endpoints_idle = terminal_endpoints_idle,
    .delay_us = delay_us,
};

static bool quiesce_still_valid(void) {
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  const bool valid = !ikb_glue_policy_failed(&glue_policy);
  k_spin_unlock(&glue_policy_lock, key);
  return valid;
}

static _Noreturn void terminal_failure(void) {
  protocol_failure();
  stop_aggregate_timer();
  (void)disable_sensor_interrupts();
  (void)k_work_queue_drain(&sensor_work_queue, true);
  (void)force_terminal_marker_low();
  uart_irq_rx_disable(cdc_device);
  for (;;) {
    k_sleep(K_FOREVER);
  }
}

static _Noreturn void finish_quiesce_and_halt(
    ikb_acquisition_state_t *acquisition,
    const ikb_acquisition_hal_t *acquisition_hal,
    uint64_t target_time_us) {
  if (!disable_sensor_interrupts()) {
    terminal_failure();
  }
  stop_aggregate_timer();
  if (k_work_queue_drain(&sensor_work_queue, true) != 0) {
    /* The safe-boundary predicate requires the queue to be empty already. */
    terminal_failure();
  }

  ikb_sensor_frame_t final_frame;
  if (!ikb_acquire_frame(acquisition, acquisition_hal, target_time_us,
                         target_time_us + IKB_AGGREGATE_PERIOD_US,
                         &final_frame)) {
    terminal_failure();
  }

  uint8_t final_transport[IKB_TRANSPORT_MAX_LEN];
  k_spinlock_key_t key = k_spin_lock(&glue_policy_lock);
  (void)ikb_glue_apply_overrun_flag(&glue_policy, &final_frame.flags);
  k_spin_unlock(&glue_policy_lock, key);
  const size_t final_transport_length = ikb_encode_transport_frame(
      &final_frame, final_transport, sizeof(final_transport));
  if (final_frame.flags != 0U ||
      final_frame.valid_sensor_mask != IKB_ALL_SENSORS_VALID ||
      final_transport_length == 0U) {
    terminal_failure();
  }

  key = k_spin_lock(&glue_policy_lock);
  const bool final_queued = ikb_glue_transport_offer_final(
      &glue_policy, final_transport, final_transport_length);
  const bool aggregate_finished = ikb_glue_aggregate_finish(&glue_policy);
  const bool shutdown_started = ikb_glue_begin_shutdown(&glue_policy);
  if (final_queued) {
    uart_irq_tx_enable(cdc_device);
  }
  k_spin_unlock(&glue_policy_lock, key);
  if (!final_queued || !aggregate_finished || !shutdown_started) {
    terminal_failure();
  }

  if (!ikb_sensor_bank_shutdown(&sensor_bus) || !quiesce_still_valid()) {
    terminal_failure();
  }

  key = k_spin_lock(&glue_policy_lock);
  const bool marking_started =
      ikb_glue_begin_marking(&glue_policy, now_us());
  k_spin_unlock(&glue_policy_lock, key);
  if (!marking_started ||
      !ikb_analyzer_emit_terminal_marker(&terminal_analyzer_marker)) {
    terminal_failure();
  }

  ikb_quiesce_request_t request;
  key = k_spin_lock(&glue_policy_lock);
  const bool marker_fell = ikb_glue_marker_fell(&glue_policy, now_us());
  const bool have_request = ikb_glue_quiesce_request(&glue_policy, &request);
  k_spin_unlock(&glue_policy_lock, key);
  if (!marker_fell || !have_request) {
    terminal_failure();
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
  const size_t receipt_transport_length =
      ikb_encode_transport_quiesce_receipt(
          &receipt, receipt_transport, sizeof(receipt_transport));
  if (receipt_transport_length != sizeof(receipt_transport)) {
    terminal_failure();
  }

  key = k_spin_lock(&glue_policy_lock);
  const bool receipt_queued = ikb_glue_transport_offer_receipt(
      &glue_policy, receipt_transport, receipt_transport_length, now_us());
  if (receipt_queued) {
    uart_irq_tx_enable(cdc_device);
  }
  k_spin_unlock(&glue_policy_lock, key);
  if (!receipt_queued) {
    terminal_failure();
  }

  uart_irq_rx_disable(cdc_device);
  for (;;) {
    k_sleep(K_FOREVER);
  }
}

static bool random_fill(void *context, uint8_t *output, size_t output_length) {
  ARG_UNUSED(context);
  return sys_csrand_get(output, output_length) == 0;
}

static bool initialize_identity(void) {
  static const uint8_t provisioned_id[] = CONFIG_IKB_CONTROLLER_ID;
  return ikb_derive_controller_id(provisioned_id,
                                  sizeof(provisioned_id) - 1U,
                                  controller_id) &&
         ikb_create_boot_id(NULL, random_fill, controller_boot_id);
}

static int initialize_sensor_inputs(void) {
  for (uint8_t sensor_index = 0U; sensor_index < IKB_SENSOR_COUNT;
       ++sensor_index) {
    sensor_context_t *sensor = &sensors[sensor_index];
    sensor->index = sensor_index;
    ikb_glue_sensor_initialize(&sensor->admission);
    if (!gpio_is_ready_dt(&sensor->drdy)) {
      return -ENODEV;
    }
    k_work_init(&sensor->work, sensor_work_handler);
    int error = gpio_pin_configure_dt(&sensor->drdy, GPIO_INPUT);
    if (error != 0) {
      return error;
    }
    gpio_init_callback(&sensor->gpio_callback, data_ready_handler,
                       BIT(sensor->drdy.pin));
    error = gpio_add_callback(sensor->drdy.port, &sensor->gpio_callback);
    if (error != 0) {
      return error;
    }
    error = gpio_pin_interrupt_configure_dt(&sensor->drdy,
                                            GPIO_INT_EDGE_TO_ACTIVE);
    if (error != 0) {
      return error;
    }
  }
  return 0;
}

int main(void) {
  if (!device_is_ready(i2c_device) || !device_is_ready(cdc_device) ||
      !gpio_is_ready_dt(&snapshot_marker) ||
      !gpio_is_ready_dt(&cache_commit_marker) ||
      !device_is_ready(gpio0)) {
    return -ENODEV;
  }
  if (snapshot_marker.port != cache_commit_marker.port ||
      gpio_pin_configure_dt(&cache_commit_marker, GPIO_OUTPUT_INACTIVE) != 0 ||
      gpio_pin_configure_dt(&snapshot_marker, GPIO_OUTPUT_INACTIVE) != 0) {
    return -EIO;
  }
  if (!ikb_analyzer_emit_startup_anchor(&snapshot_analyzer_marker,
                                        &cache_commit_analyzer_marker)) {
    return -EIO;
  }

  ikb_sample_cache_initialize(&sample_cache);
  ikb_glue_policy_initialize(&glue_policy);
  k_work_queue_init(&sensor_work_queue);
  k_work_queue_start(&sensor_work_queue, sensor_work_queue_stack,
                     K_THREAD_STACK_SIZEOF(sensor_work_queue_stack),
                     SENSOR_WORK_QUEUE_PRIORITY, NULL);

  k_msleep(SENSOR_STARTUP_DELAY_MS);
  if (!ikb_sensor_bank_initialize(&sensor_bus) ||
      initialize_sensor_inputs() != 0 || !initialize_identity()) {
    return -EIO;
  }
  k_work_init(&sensor_bank_arm_work, sensor_bank_arm_work_handler);
  if (k_work_submit_to_queue(&sensor_work_queue, &sensor_bank_arm_work) != 1 ||
      k_sem_take(&sensor_bank_arm_sem, K_FOREVER) != 0 ||
      !sensor_bank_arm_ok) {
    return -EIO;
  }
  if (!prime_sensor_cache()) {
    return -EIO;
  }

  controller_hand = IS_ENABLED(CONFIG_IKB_HAND_RIGHT) ? IKB_HAND_RIGHT
                                                       : IKB_HAND_LEFT;
  ikb_acquisition_state_t acquisition;
  if (!ikb_acquisition_init(&acquisition, controller_hand, controller_id,
                            controller_boot_id) ||
      uart_irq_callback_user_data_set(cdc_device, cdc_interrupt_handler,
                                      NULL) != 0) {
    return -EIO;
  }
  uart_irq_err_enable(cdc_device);
  uart_irq_rx_enable(cdc_device);

  aggregate_snapshot_t aggregate_snapshot = {0};
  const ikb_acquisition_hal_t acquisition_hal = {
      .context = &aggregate_snapshot,
      .read_latest = read_snapshot_sample,
      .now_us = acquisition_now_us,
  };
  k_timer_init(&aggregate_timer, aggregate_timer_handler, NULL);
  uint64_t target_time_us = now_us();
  k_spinlock_key_t timer_key = k_spin_lock(&glue_policy_lock);
  aggregate_timer_target_time_us = target_time_us;
  aggregate_timer_active = true;
  arm_next_aggregate();
  k_spin_unlock(&glue_policy_lock, timer_key);
  for (;;) {
    k_sem_take(&aggregate_sem, K_FOREVER);
    uint32_t elapsed_periods = 0U;
    k_spinlock_key_t glue_key = k_spin_lock(&glue_policy_lock);
    const bool aggregate_started =
        ikb_glue_aggregate_begin(&glue_policy, &elapsed_periods);
    k_spin_unlock(&glue_policy_lock, glue_key);
    if (!aggregate_started) {
      continue;
    }
    target_time_us +=
        (uint64_t)elapsed_periods * IKB_AGGREGATE_PERIOD_US;

    const final_boundary_result_t boundary =
        try_final_boundary(&aggregate_snapshot, target_time_us);
    if (boundary == FINAL_BOUNDARY_FAILED) {
      terminal_failure();
    }
    if (boundary == FINAL_BOUNDARY_CLOSED) {
      finish_quiesce_and_halt(&acquisition, &acquisition_hal,
                              target_time_us);
    }

    glue_key = k_spin_lock(&glue_policy_lock);
    k_spinlock_key_t snapshot_key = k_spin_lock(&sample_cache_lock);
    const bool snapshot_ok = ikb_sample_cache_snapshot(
        &sample_cache, aggregate_snapshot.samples, &snapshot_analyzer_marker);
    k_spin_unlock(&sample_cache_lock, snapshot_key);
    k_spin_unlock(&glue_policy_lock, glue_key);
    if (!snapshot_ok) {
      return -EIO;
    }

    ikb_sensor_frame_t frame;
    if (!ikb_acquire_frame(&acquisition, &acquisition_hal, target_time_us,
                           target_time_us + IKB_AGGREGATE_PERIOD_US, &frame)) {
      return -EIO;
    }
    glue_key = k_spin_lock(&glue_policy_lock);
    (void)ikb_glue_apply_overrun_flag(&glue_policy, &frame.flags);
    k_spin_unlock(&glue_policy_lock, glue_key);

    uint8_t transport[IKB_TRANSPORT_MAX_LEN];
    const size_t transport_length =
        ikb_encode_transport_frame(&frame, transport, sizeof(transport));
    if (transport_length == 0U) {
      return -EIO;
    }
    (void)queue_normal_record(transport, transport_length);
    glue_key = k_spin_lock(&glue_policy_lock);
    const bool aggregate_finished =
        ikb_glue_aggregate_finish(&glue_policy);
    k_spin_unlock(&glue_policy_lock, glue_key);
    if (!aggregate_finished) {
      return -EIO;
    }
  }
}
