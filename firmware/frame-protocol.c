#include "firmware/frame-protocol.h"

#include <string.h>

enum {
  IKB_CONTROLLER_ID_OFFSET = 24,
  IKB_BOOT_ID_OFFSET = 56,
  IKB_HEADER_LEN = 72,
  IKB_CHECKSUM_OFFSET = IKB_FRAME_LEN - 4,
  IKB_QUIESCE_CONTROLLER_ID_OFFSET = 8,
  IKB_QUIESCE_BOOT_ID_OFFSET = 40,
  IKB_QUIESCE_SESSION_ID_OFFSET = 56,
  IKB_QUIESCE_REQUEST_TIME_OFFSET = 72,
  IKB_QUIESCE_REQUEST_CHECKSUM_OFFSET = IKB_QUIESCE_REQUEST_LEN - 4,
  IKB_QUIESCE_RECEIPT_SEQUENCE_OFFSET = 80,
  IKB_QUIESCE_RECEIPT_CHECKSUM_OFFSET = IKB_QUIESCE_RECEIPT_LEN - 4,
  IKB_QUIESCE_DISCRIMINATOR = 1,
};

static const uint8_t IKB_FRAME_MAGIC[4] = {'I', 'K', 'B', '2'};
static const uint8_t IKB_QUIESCE_REQUEST_MAGIC[4] = {'I', 'K', 'Q', '2'};
static const uint8_t IKB_QUIESCE_RECEIPT_MAGIC[4] = {'I', 'K', 'R', '2'};

static void put_u16_le(uint8_t *output, size_t offset, uint16_t value) {
  output[offset] = (uint8_t)(value & 0xffU);
  output[offset + 1U] = (uint8_t)((value >> 8U) & 0xffU);
}

static void put_u32_le(uint8_t *output, size_t offset, uint32_t value) {
  for (size_t byte = 0; byte < 4U; ++byte) {
    output[offset + byte] = (uint8_t)((value >> (byte * 8U)) & 0xffU);
  }
}

static void put_u64_le(uint8_t *output, size_t offset, uint64_t value) {
  for (size_t byte = 0; byte < 8U; ++byte) {
    output[offset + byte] = (uint8_t)((value >> (byte * 8U)) & 0xffU);
  }
}

static uint32_t read_u32_le(const uint8_t *input, size_t offset) {
  uint32_t value = 0U;
  for (size_t byte = 0U; byte < 4U; ++byte) {
    value |= (uint32_t)input[offset + byte] << (byte * 8U);
  }
  return value;
}

static uint64_t read_u64_le(const uint8_t *input, size_t offset) {
  uint64_t value = 0U;
  for (size_t byte = 0U; byte < 8U; ++byte) {
    value |= (uint64_t)input[offset + byte] << (byte * 8U);
  }
  return value;
}

uint32_t ikb_crc32(const uint8_t *bytes, size_t length) {
  uint32_t crc = UINT32_MAX;
  for (size_t index = 0; index < length; ++index) {
    crc ^= bytes[index];
    for (size_t bit = 0; bit < 8U; ++bit) {
      const uint32_t mask = 0U - (crc & 1U);
      crc = (crc >> 1U) ^ (0xedb88320U & mask);
    }
  }
  return ~crc;
}

static bool valid_hand(ikb_hand_t hand) {
  return hand == IKB_HAND_LEFT || hand == IKB_HAND_RIGHT;
}

static bool valid_quiesce_identity(const uint8_t *controller_id_sha256,
                                    const uint8_t *boot_id,
                                    const uint8_t *session_id) {
  return ikb_has_nonzero_byte(controller_id_sha256,
                              IKB_CONTROLLER_ID_SHA256_LEN) &&
         ikb_has_nonzero_byte(boot_id, IKB_BOOT_ID_LEN) &&
         ikb_has_nonzero_byte(session_id, IKB_SESSION_ID_LEN);
}

static void encode_quiesce_header(uint8_t *output, const uint8_t magic[4],
                                   ikb_hand_t hand,
                                   const uint8_t *controller_id_sha256,
                                   const uint8_t *boot_id,
                                   const uint8_t *session_id,
                                   uint64_t request_start_time_ns) {
  memcpy(output, magic, 4U);
  output[4] = IKB_PROTOCOL_VERSION;
  output[5] = (uint8_t)hand;
  output[6] = IKB_QUIESCE_DISCRIMINATOR;
  memcpy(output + IKB_QUIESCE_CONTROLLER_ID_OFFSET, controller_id_sha256,
         IKB_CONTROLLER_ID_SHA256_LEN);
  memcpy(output + IKB_QUIESCE_BOOT_ID_OFFSET, boot_id, IKB_BOOT_ID_LEN);
  memcpy(output + IKB_QUIESCE_SESSION_ID_OFFSET, session_id,
         IKB_SESSION_ID_LEN);
  put_u64_le(output, IKB_QUIESCE_REQUEST_TIME_OFFSET,
             request_start_time_ns);
}

static bool decode_quiesce_header(const uint8_t *input, size_t input_length,
                                   size_t expected_length,
                                   size_t checksum_offset,
                                   const uint8_t expected_magic[4],
                                   ikb_hand_t *hand) {
  if (input == NULL || hand == NULL || input_length != expected_length) {
    return false;
  }
  const uint32_t stored_checksum = read_u32_le(input, checksum_offset);
  if (stored_checksum != ikb_crc32(input, checksum_offset) ||
      memcmp(input, expected_magic, 4U) != 0 ||
      input[4] != IKB_PROTOCOL_VERSION ||
      input[6] != IKB_QUIESCE_DISCRIMINATOR || input[7] != 0U ||
      !valid_hand((ikb_hand_t)input[5])) {
    return false;
  }
  if (!valid_quiesce_identity(input + IKB_QUIESCE_CONTROLLER_ID_OFFSET,
                               input + IKB_QUIESCE_BOOT_ID_OFFSET,
                               input + IKB_QUIESCE_SESSION_ID_OFFSET)) {
    return false;
  }
  *hand = (ikb_hand_t)input[5];
  return true;
}

bool ikb_encode_quiesce_request(const ikb_quiesce_request_t *request,
                                uint8_t *output, size_t output_length) {
  if (request == NULL || output == NULL ||
      output_length != IKB_QUIESCE_REQUEST_LEN ||
      !valid_hand(request->hand) ||
      !valid_quiesce_identity(request->controller_id_sha256, request->boot_id,
                              request->session_id)) {
    return false;
  }
  encode_quiesce_header(output, IKB_QUIESCE_REQUEST_MAGIC, request->hand,
                         request->controller_id_sha256, request->boot_id,
                         request->session_id,
                         request->request_start_time_ns);
  output[7] = 0U;
  put_u32_le(output, IKB_QUIESCE_REQUEST_CHECKSUM_OFFSET,
             ikb_crc32(output, IKB_QUIESCE_REQUEST_CHECKSUM_OFFSET));
  return true;
}

bool ikb_decode_quiesce_request(const uint8_t *input, size_t input_length,
                                ikb_quiesce_request_t *request) {
  ikb_hand_t hand = IKB_HAND_LEFT;
  if (request == NULL ||
      !decode_quiesce_header(input, input_length, IKB_QUIESCE_REQUEST_LEN,
                              IKB_QUIESCE_REQUEST_CHECKSUM_OFFSET,
                              IKB_QUIESCE_REQUEST_MAGIC, &hand)) {
    return false;
  }
  ikb_quiesce_request_t decoded = {0};
  decoded.hand = hand;
  memcpy(decoded.controller_id_sha256,
         input + IKB_QUIESCE_CONTROLLER_ID_OFFSET,
         sizeof(decoded.controller_id_sha256));
  memcpy(decoded.boot_id, input + IKB_QUIESCE_BOOT_ID_OFFSET,
         sizeof(decoded.boot_id));
  memcpy(decoded.session_id, input + IKB_QUIESCE_SESSION_ID_OFFSET,
         sizeof(decoded.session_id));
  decoded.request_start_time_ns =
      read_u64_le(input, IKB_QUIESCE_REQUEST_TIME_OFFSET);
  *request = decoded;
  return true;
}

bool ikb_encode_quiesce_receipt(const ikb_quiesce_receipt_t *receipt,
                                uint8_t *output, size_t output_length) {
  if (receipt == NULL || output == NULL ||
      output_length != IKB_QUIESCE_RECEIPT_LEN ||
      !valid_hand(receipt->hand) ||
      !valid_quiesce_identity(receipt->controller_id_sha256, receipt->boot_id,
                              receipt->session_id)) {
    return false;
  }
  encode_quiesce_header(output, IKB_QUIESCE_RECEIPT_MAGIC, receipt->hand,
                         receipt->controller_id_sha256, receipt->boot_id,
                         receipt->session_id,
                         receipt->request_start_time_ns);
  output[7] = 0U;
  put_u32_le(output, IKB_QUIESCE_RECEIPT_SEQUENCE_OFFSET,
             receipt->final_sequence);
  put_u32_le(output, IKB_QUIESCE_RECEIPT_CHECKSUM_OFFSET,
             ikb_crc32(output, IKB_QUIESCE_RECEIPT_CHECKSUM_OFFSET));
  return true;
}

bool ikb_decode_quiesce_receipt(const uint8_t *input, size_t input_length,
                                ikb_quiesce_receipt_t *receipt) {
  ikb_hand_t hand = IKB_HAND_LEFT;
  if (receipt == NULL ||
      !decode_quiesce_header(input, input_length, IKB_QUIESCE_RECEIPT_LEN,
                              IKB_QUIESCE_RECEIPT_CHECKSUM_OFFSET,
                              IKB_QUIESCE_RECEIPT_MAGIC, &hand)) {
    return false;
  }
  ikb_quiesce_receipt_t decoded = {0};
  decoded.hand = hand;
  memcpy(decoded.controller_id_sha256,
         input + IKB_QUIESCE_CONTROLLER_ID_OFFSET,
         sizeof(decoded.controller_id_sha256));
  memcpy(decoded.boot_id, input + IKB_QUIESCE_BOOT_ID_OFFSET,
         sizeof(decoded.boot_id));
  memcpy(decoded.session_id, input + IKB_QUIESCE_SESSION_ID_OFFSET,
         sizeof(decoded.session_id));
  decoded.request_start_time_ns =
      read_u64_le(input, IKB_QUIESCE_REQUEST_TIME_OFFSET);
  decoded.final_sequence =
      read_u32_le(input, IKB_QUIESCE_RECEIPT_SEQUENCE_OFFSET);
  *receipt = decoded;
  return true;
}

bool ikb_encode_frame(const ikb_sensor_frame_t *frame, uint8_t *output,
                      size_t output_length) {
  if (frame == NULL || output == NULL || output_length != IKB_FRAME_LEN) {
    return false;
  }
  if (!valid_hand(frame->hand)) {
    return false;
  }
  if ((frame->valid_sensor_mask & (uint8_t)~IKB_ALL_SENSORS_VALID) != 0U ||
      (frame->flags & ~IKB_KNOWN_FLAGS) != 0U) {
    return false;
  }
  if (!ikb_has_nonzero_byte(frame->controller_id_sha256,
                            sizeof(frame->controller_id_sha256)) ||
      !ikb_has_nonzero_byte(frame->boot_id, sizeof(frame->boot_id))) {
    return false;
  }

  memcpy(output, IKB_FRAME_MAGIC, sizeof(IKB_FRAME_MAGIC));
  output[4] = IKB_PROTOCOL_VERSION;
  output[5] = (uint8_t)frame->hand;
  output[6] = IKB_SENSOR_COUNT;
  output[7] = frame->valid_sensor_mask;
  put_u32_le(output, 8U, frame->flags);
  put_u32_le(output, 12U, frame->sequence);
  put_u64_le(output, 16U, frame->device_time_us);
  memcpy(output + IKB_CONTROLLER_ID_OFFSET, frame->controller_id_sha256,
         sizeof(frame->controller_id_sha256));
  memcpy(output + IKB_BOOT_ID_OFFSET, frame->boot_id,
         sizeof(frame->boot_id));

  size_t offset = IKB_HEADER_LEN;
  for (size_t sensor = 0; sensor < IKB_SENSOR_COUNT; ++sensor) {
    for (size_t axis = 0; axis < 3U; ++axis) {
      put_u16_le(output, offset,
                 (uint16_t)frame->sensors[sensor].acceleration[axis]);
      offset += 2U;
    }
    for (size_t axis = 0; axis < 3U; ++axis) {
      put_u16_le(output, offset,
                 (uint16_t)frame->sensors[sensor].angular_velocity[axis]);
      offset += 2U;
    }
    put_u16_le(output, offset,
               (uint16_t)frame->sensors[sensor].sample_time_offset_us);
    offset += 2U;
  }
  if (offset != IKB_CHECKSUM_OFFSET) {
    return false;
  }
  put_u32_le(output, IKB_CHECKSUM_OFFSET,
             ikb_crc32(output, IKB_CHECKSUM_OFFSET));
  return true;
}

static size_t encode_transport_record(const uint8_t *raw, size_t raw_length,
                                      uint8_t *output,
                                      size_t output_capacity) {
  if (raw == NULL || output == NULL || raw_length == 0U ||
      raw_length >= UINT8_MAX || output_capacity < raw_length + 2U) {
    return 0U;
  }
  size_t write_offset = 1U;
  size_t code_offset = 0U;
  uint8_t code = 1U;
  output[0] = 0U;

  for (size_t read_offset = 0U; read_offset < raw_length; ++read_offset) {
    if (raw[read_offset] == 0U) {
      output[code_offset] = code;
      code_offset = write_offset;
      output[write_offset++] = 0U;
      code = 1U;
      continue;
    }

    output[write_offset++] = raw[read_offset];
    ++code;
    if (code == UINT8_MAX) {
      output[code_offset] = code;
      code_offset = write_offset;
      output[write_offset++] = 0U;
      code = 1U;
    }
  }

  output[code_offset] = code;
  output[write_offset++] = 0U;
  return write_offset;
}

size_t ikb_encode_transport_frame(const ikb_sensor_frame_t *frame,
                                  uint8_t *output, size_t output_capacity) {
  uint8_t raw[IKB_FRAME_LEN];
  if (output_capacity < IKB_TRANSPORT_MAX_LEN ||
      !ikb_encode_frame(frame, raw, sizeof(raw))) {
    return 0U;
  }
  return encode_transport_record(raw, sizeof(raw), output, output_capacity);
}

size_t ikb_encode_transport_quiesce_request(
    const ikb_quiesce_request_t *request, uint8_t *output,
    size_t output_capacity) {
  uint8_t raw[IKB_QUIESCE_REQUEST_LEN];
  if (output_capacity < IKB_QUIESCE_REQUEST_TRANSPORT_LEN ||
      !ikb_encode_quiesce_request(request, raw, sizeof(raw))) {
    return 0U;
  }
  return encode_transport_record(raw, sizeof(raw), output, output_capacity);
}

size_t ikb_encode_transport_quiesce_receipt(
    const ikb_quiesce_receipt_t *receipt, uint8_t *output,
    size_t output_capacity) {
  uint8_t raw[IKB_QUIESCE_RECEIPT_LEN];
  if (output_capacity < IKB_QUIESCE_RECEIPT_TRANSPORT_LEN ||
      !ikb_encode_quiesce_receipt(receipt, raw, sizeof(raw))) {
    return 0U;
  }
  return encode_transport_record(raw, sizeof(raw), output, output_capacity);
}

size_t ikb_decode_transport_record(const uint8_t *input, size_t input_length,
                                   uint8_t *output, size_t output_capacity) {
  if (input == NULL || output == NULL || input_length == 0U) {
    return 0U;
  }

  size_t read_offset = 0U;
  size_t write_offset = 0U;
  while (read_offset < input_length) {
    const size_t code = input[read_offset++];
    if (code == 0U) {
      return 0U;
    }
    const size_t run_length = code - 1U;
    if (run_length > input_length - read_offset ||
        run_length > output_capacity - write_offset) {
      return 0U;
    }
    memcpy(output + write_offset, input + read_offset, run_length);
    read_offset += run_length;
    write_offset += run_length;

    if (code != UINT8_MAX && read_offset < input_length) {
      if (write_offset == output_capacity) {
        return 0U;
      }
      output[write_offset++] = 0U;
    }
  }
  return write_offset;
}
