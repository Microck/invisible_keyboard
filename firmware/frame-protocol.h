#ifndef INVISIBLE_KEYBOARD_FRAME_PROTOCOL_H_
#define INVISIBLE_KEYBOARD_FRAME_PROTOCOL_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define IKB_PROTOCOL_VERSION 2U
#define IKB_SENSOR_COUNT 6U
#define IKB_CONTROLLER_ID_SHA256_LEN 32U
#define IKB_BOOT_ID_LEN 16U
#define IKB_SESSION_ID_LEN 16U
#define IKB_FRAME_LEN 160U
#define IKB_TRANSPORT_MAX_LEN 162U
#define IKB_QUIESCE_REQUEST_LEN 84U
#define IKB_QUIESCE_RECEIPT_LEN 88U
#define IKB_QUIESCE_REQUEST_ENCODED_LEN (IKB_QUIESCE_REQUEST_LEN + 1U)
#define IKB_QUIESCE_RECEIPT_ENCODED_LEN (IKB_QUIESCE_RECEIPT_LEN + 1U)
#define IKB_QUIESCE_REQUEST_TRANSPORT_LEN                               \
  (IKB_QUIESCE_REQUEST_ENCODED_LEN + 1U)
#define IKB_QUIESCE_RECEIPT_TRANSPORT_LEN                               \
  (IKB_QUIESCE_RECEIPT_ENCODED_LEN + 1U)
#define IKB_ALL_SENSORS_VALID 0x3fU

#define IKB_FLAG_CLOCK_UNSYNCED (1UL << 1U)
#define IKB_FLAG_ACQUISITION_OVERRUN (1UL << 2U)
#define IKB_KNOWN_FLAGS                                                        \
  (IKB_FLAG_CLOCK_UNSYNCED | IKB_FLAG_ACQUISITION_OVERRUN)

typedef enum {
  IKB_HAND_LEFT = 0,
  IKB_HAND_RIGHT = 1,
} ikb_hand_t;

typedef struct {
  int16_t acceleration[3];
  int16_t angular_velocity[3];
  int16_t sample_time_offset_us;
} ikb_sensor_reading_t;

typedef struct {
  ikb_hand_t hand;
  uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN];
  uint8_t boot_id[IKB_BOOT_ID_LEN];
  uint32_t flags;
  uint8_t valid_sensor_mask;
  uint32_t sequence;
  uint64_t device_time_us;
  ikb_sensor_reading_t sensors[IKB_SENSOR_COUNT];
} ikb_sensor_frame_t;

typedef struct {
  ikb_hand_t hand;
  uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN];
  uint8_t boot_id[IKB_BOOT_ID_LEN];
  uint8_t session_id[IKB_SESSION_ID_LEN];
  uint64_t request_start_time_ns;
} ikb_quiesce_request_t;

typedef struct {
  ikb_hand_t hand;
  uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN];
  uint8_t boot_id[IKB_BOOT_ID_LEN];
  uint8_t session_id[IKB_SESSION_ID_LEN];
  uint64_t request_start_time_ns;
  uint32_t final_sequence;
} ikb_quiesce_receipt_t;

/* Return true when a validated byte buffer contains any nonzero byte. */
static inline bool ikb_has_nonzero_byte(const uint8_t *bytes, size_t length) {
  for (size_t index = 0U; index < length; ++index) {
    if (bytes[index] != 0U) {
      return true;
    }
  }
  return false;
}

/* IEEE CRC-32 using the reflected 0xedb88320 polynomial. */
uint32_t ikb_crc32(const uint8_t *bytes, size_t length);

/*
 * Encode one version 2 frame without casting a C struct onto the wire.
 *
 * Returns false for a null pointer, a buffer not exactly IKB_FRAME_LEN bytes,
 * an invalid hand, zero physical identity, validity bits outside the six
 * channels, or unknown flags.
 */
bool ikb_encode_frame(const ikb_sensor_frame_t *frame, uint8_t *output,
                      size_t output_length);

/* Encode one frame with COBS and append its zero stream delimiter. */
size_t ikb_encode_transport_frame(const ikb_sensor_frame_t *frame,
                                  uint8_t *output, size_t output_capacity);

/* Encode or decode exact decoded IKQ2 and IKR2 records. */
bool ikb_encode_quiesce_request(const ikb_quiesce_request_t *request,
                                uint8_t *output, size_t output_length);
bool ikb_decode_quiesce_request(const uint8_t *input, size_t input_length,
                                ikb_quiesce_request_t *request);
bool ikb_encode_quiesce_receipt(const ikb_quiesce_receipt_t *receipt,
                                uint8_t *output, size_t output_length);
bool ikb_decode_quiesce_receipt(const uint8_t *input, size_t input_length,
                                ikb_quiesce_receipt_t *receipt);

/* Encode the exact control record with COBS and append its zero delimiter. */
size_t ikb_encode_transport_quiesce_request(
    const ikb_quiesce_request_t *request, uint8_t *output,
    size_t output_capacity);
size_t ikb_encode_transport_quiesce_receipt(
    const ikb_quiesce_receipt_t *receipt, uint8_t *output,
    size_t output_capacity);

/*
 * Decode one COBS payload without its zero delimiter.
 *
 * Returns the decoded length, or zero for null pointers, an empty input,
 * malformed COBS, or insufficient output capacity.
 */
size_t ikb_decode_transport_record(const uint8_t *input, size_t input_length,
                                   uint8_t *output, size_t output_capacity);

#endif
