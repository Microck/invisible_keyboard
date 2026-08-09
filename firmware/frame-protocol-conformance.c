#include "firmware/frame-protocol.h"

#include <stdio.h>
#include <string.h>

static bool expect(bool condition, const char *message) {
  if (condition) {
    return true;
  }
  fprintf(stderr, "conformance failure: %s\n", message);
  return false;
}

static int hex_nibble(char value) {
  if (value >= '0' && value <= '9') {
    return value - '0';
  }
  if (value >= 'a' && value <= 'f') {
    return value - 'a' + 10;
  }
  return -1;
}

static bool decode_hex_exact(const char *source, uint8_t *output,
                             size_t output_length) {
  if (source == NULL || output == NULL ||
      strlen(source) != output_length * 2U) {
    return false;
  }
  for (size_t index = 0U; index < output_length; ++index) {
    const int high = hex_nibble(source[index * 2U]);
    const int low = hex_nibble(source[index * 2U + 1U]);
    if (high < 0 || low < 0) {
      return false;
    }
    output[index] = (uint8_t)((high << 4) | low);
  }
  return true;
}

static bool request_equal(const ikb_quiesce_request_t *left,
                          const ikb_quiesce_request_t *right) {
  return left->hand == right->hand &&
         memcmp(left->controller_id_sha256, right->controller_id_sha256,
                sizeof(left->controller_id_sha256)) == 0 &&
         memcmp(left->boot_id, right->boot_id, sizeof(left->boot_id)) == 0 &&
         memcmp(left->session_id, right->session_id,
                sizeof(left->session_id)) == 0 &&
         left->request_start_time_ns == right->request_start_time_ns;
}

static bool receipt_equal(const ikb_quiesce_receipt_t *left,
                          const ikb_quiesce_receipt_t *right) {
  return left->hand == right->hand &&
         memcmp(left->controller_id_sha256, right->controller_id_sha256,
                sizeof(left->controller_id_sha256)) == 0 &&
         memcmp(left->boot_id, right->boot_id, sizeof(left->boot_id)) == 0 &&
         memcmp(left->session_id, right->session_id,
                sizeof(left->session_id)) == 0 &&
         left->request_start_time_ns == right->request_start_time_ns &&
         left->final_sequence == right->final_sequence;
}

int main(void) {
  static const uint8_t crc_check[] = "123456789";
  ikb_sensor_frame_t frame = {0};
  uint8_t encoded[IKB_FRAME_LEN] = {0};
  uint8_t transport[IKB_TRANSPORT_MAX_LEN] = {0};

  ikb_quiesce_request_t request = {0};
  request.hand = IKB_HAND_LEFT;
  memset(request.controller_id_sha256, 0x11,
         sizeof(request.controller_id_sha256));
  memset(request.boot_id, 0x22, sizeof(request.boot_id));
  memset(request.session_id, 0x33, sizeof(request.session_id));
  request.request_start_time_ns = UINT64_C(0x0102030405060708);
  uint8_t encoded_request[IKB_QUIESCE_REQUEST_LEN] = {0};
  uint8_t request_transport[IKB_QUIESCE_REQUEST_TRANSPORT_LEN] = {0};
  uint8_t decoded_request_bytes[IKB_QUIESCE_REQUEST_LEN] = {0};
  uint8_t expected_request[IKB_QUIESCE_REQUEST_LEN] = {0};
  ikb_quiesce_request_t decoded_request = {0};

  ikb_quiesce_receipt_t receipt = {0};
  receipt.hand = request.hand;
  memcpy(receipt.controller_id_sha256, request.controller_id_sha256,
         sizeof(receipt.controller_id_sha256));
  memcpy(receipt.boot_id, request.boot_id, sizeof(receipt.boot_id));
  memcpy(receipt.session_id, request.session_id, sizeof(receipt.session_id));
  receipt.request_start_time_ns = request.request_start_time_ns;
  receipt.final_sequence = UINT32_C(0x12345678);
  uint8_t encoded_receipt[IKB_QUIESCE_RECEIPT_LEN] = {0};
  uint8_t receipt_transport[IKB_QUIESCE_RECEIPT_TRANSPORT_LEN] = {0};
  uint8_t decoded_receipt_bytes[IKB_QUIESCE_RECEIPT_LEN] = {0};
  uint8_t expected_receipt[IKB_QUIESCE_RECEIPT_LEN] = {0};
  ikb_quiesce_receipt_t decoded_receipt = {0};

  if (!expect(ikb_crc32(crc_check, sizeof(crc_check) - 1U) == 0xcbf43926U,
              "CRC-32 standard check value") ||
      !expect(!ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN - 1U),
              "short output buffer rejected") ||
      !expect(ikb_encode_transport_frame(&frame, transport,
                                         IKB_TRANSPORT_MAX_LEN - 1U) == 0U,
              "short transport buffer rejected")) {
    return 1;
  }

  if (!expect(decode_hex_exact(
                  "494b51320200010011111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222233333333333333333333333333333333080706050403020146da2f27",
                  expected_request, sizeof(expected_request)),
              "request golden decoded") ||
      !expect(decode_hex_exact(
                  "494b523202000100111111111111111111111111111111111111111111111111111111111111111122222222222222222222222222222222333333333333333333333333333333330807060504030201785634125d98c9a6",
                  expected_receipt, sizeof(expected_receipt)),
              "receipt golden decoded") ||
      !expect(ikb_encode_quiesce_request(&request, encoded_request,
                                          sizeof(encoded_request)),
              "quiesce request encoded") ||
      !expect(memcmp(encoded_request, expected_request,
                     sizeof(expected_request)) == 0,
              "quiesce request exact golden") ||
      !expect(memcmp(encoded_request, "IKQ2\x02\x00\x01\x00", 8U) == 0,
              "quiesce request fixed header") ||
      !expect(ikb_decode_quiesce_request(encoded_request,
                                          sizeof(encoded_request),
                                          &decoded_request),
              "quiesce request decoded") ||
      !expect(request_equal(&request, &decoded_request),
              "quiesce request round trip") ||
      !expect(!ikb_decode_quiesce_request(encoded_request,
                                           sizeof(encoded_request) - 1U,
                                           &decoded_request),
              "short quiesce request rejected") ||
      !expect(ikb_encode_quiesce_receipt(&receipt, encoded_receipt,
                                          sizeof(encoded_receipt)),
              "quiesce receipt encoded") ||
      !expect(memcmp(encoded_receipt, expected_receipt,
                     sizeof(expected_receipt)) == 0,
              "quiesce receipt exact golden") ||
      !expect(memcmp(encoded_receipt, "IKR2\x02\x00\x01\x00", 8U) == 0,
              "quiesce receipt fixed header") ||
      !expect(ikb_decode_quiesce_receipt(encoded_receipt,
                                          sizeof(encoded_receipt),
                                          &decoded_receipt),
              "quiesce receipt decoded") ||
      !expect(receipt_equal(&receipt, &decoded_receipt),
              "quiesce receipt round trip") ||
      !expect(ikb_encode_transport_quiesce_request(
                  &request, request_transport, sizeof(request_transport)) ==
                  IKB_QUIESCE_REQUEST_TRANSPORT_LEN,
              "quiesce request exact transport length") ||
      !expect(request_transport[sizeof(request_transport) - 1U] == 0U,
              "quiesce request transport delimiter") ||
      !expect(ikb_decode_transport_record(
                  request_transport, sizeof(request_transport) - 1U,
                  decoded_request_bytes, sizeof(decoded_request_bytes)) ==
                  IKB_QUIESCE_REQUEST_LEN,
              "quiesce request transport decoded") ||
      !expect(memcmp(decoded_request_bytes, encoded_request,
                     sizeof(encoded_request)) == 0,
              "quiesce request transport preserves bytes") ||
      !expect(ikb_encode_transport_quiesce_receipt(
                  &receipt, receipt_transport, sizeof(receipt_transport)) ==
                  IKB_QUIESCE_RECEIPT_TRANSPORT_LEN,
              "quiesce receipt exact transport length") ||
      !expect(ikb_decode_transport_record(
                  receipt_transport, sizeof(receipt_transport) - 1U,
                  decoded_receipt_bytes, sizeof(decoded_receipt_bytes)) ==
                  IKB_QUIESCE_RECEIPT_LEN,
              "quiesce receipt transport decoded") ||
      !expect(memcmp(decoded_receipt_bytes, encoded_receipt,
                     sizeof(encoded_receipt)) == 0,
              "quiesce receipt transport preserves bytes")) {
    return 1;
  }

  encoded_request[7] = 1U;
  encoded_request[IKB_QUIESCE_REQUEST_LEN - 4U] = 0U;
  const uint32_t reserved_checksum =
      ikb_crc32(encoded_request, IKB_QUIESCE_REQUEST_LEN - 4U);
  for (size_t byte = 0U; byte < 4U; ++byte) {
    encoded_request[IKB_QUIESCE_REQUEST_LEN - 4U + byte] =
        (uint8_t)((reserved_checksum >> (byte * 8U)) & 0xffU);
  }
  if (!expect(!ikb_decode_quiesce_request(encoded_request,
                                           sizeof(encoded_request),
                                           &decoded_request),
              "checksum-valid reserved byte rejected") ||
      !expect(ikb_decode_transport_record((const uint8_t[]){4U, 1U, 2U}, 3U,
                                           decoded_request_bytes,
                                           sizeof(decoded_request_bytes)) == 0U,
              "malformed COBS rejected")) {
    return 1;
  }

  memset(request.session_id, 0, sizeof(request.session_id));
  if (!expect(!ikb_encode_quiesce_request(&request, encoded_request,
                                           sizeof(encoded_request)),
              "zero quiesce session rejected")) {
    return 1;
  }

  frame.hand = IKB_HAND_LEFT;
  memset(frame.controller_id_sha256, 0x11, sizeof(frame.controller_id_sha256));
  memset(frame.boot_id, 0x22, sizeof(frame.boot_id));
  frame.valid_sensor_mask = IKB_ALL_SENSORS_VALID;
  frame.flags = 1UL << 0U;
  if (!expect(!ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "removed FIFO flag rejected")) {
    return 1;
  }
  frame.flags = 1UL << 31U;
  if (!expect(!ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "unknown flags rejected")) {
    return 1;
  }
  frame.flags = 0U;
  frame.valid_sensor_mask = 0x40U;
  if (!expect(!ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "out-of-range sensor mask rejected")) {
    return 1;
  }
  frame.valid_sensor_mask = IKB_ALL_SENSORS_VALID;
  frame.sensors[0].sample_time_offset_us = -2;
  frame.sensors[5].sample_time_offset_us = 0x1234;
  if (!expect(ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "signed sample offsets encoded") ||
      !expect(encoded[84] == 0xfeU && encoded[85] == 0xffU,
              "negative sample offset bytes") ||
      !expect(encoded[154] == 0x34U && encoded[155] == 0x12U,
              "positive sample offset bytes")) {
    return 1;
  }
  frame.sensors[0].sample_time_offset_us = 0;
  frame.sensors[5].sample_time_offset_us = 0;

  if (!expect(ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "canonical zero frame encoded") ||
      !expect(memcmp(encoded, "IKB2", 4U) == 0, "magic bytes") ||
      !expect(encoded[4] == IKB_PROTOCOL_VERSION, "protocol version") ||
      !expect(encoded[5] == IKB_HAND_LEFT, "hand value") ||
      !expect(encoded[6] == IKB_SENSOR_COUNT, "sensor count") ||
      !expect(memcmp(encoded + 24U, frame.controller_id_sha256,
                     sizeof(frame.controller_id_sha256)) == 0,
              "controller identity bytes") ||
      !expect(memcmp(encoded + 56U, frame.boot_id, sizeof(frame.boot_id)) == 0,
              "boot identity bytes")) {
    return 1;
  }

  memset(frame.controller_id_sha256, 0, sizeof(frame.controller_id_sha256));
  if (!expect(!ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "zero controller identity rejected")) {
    return 1;
  }
  memset(frame.controller_id_sha256, 0x11, sizeof(frame.controller_id_sha256));
  memset(frame.boot_id, 0, sizeof(frame.boot_id));
  if (!expect(!ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "zero boot identity rejected")) {
    return 1;
  }
  memset(frame.boot_id, 0x22, sizeof(frame.boot_id));
  if (!expect(ikb_encode_frame(&frame, encoded, IKB_FRAME_LEN),
              "identity-restored frame encoded")) {
    return 1;
  }

  printf("frame_length=%u\n", IKB_FRAME_LEN);
  printf("frame_hex=");
  for (size_t index = 0; index < IKB_FRAME_LEN; ++index) {
    printf("%02x", encoded[index]);
  }
  printf("\n");
  const size_t transport_length = ikb_encode_transport_frame(
      &frame, transport, sizeof(transport));
  if (!expect(transport_length > 0U &&
                  transport_length <= IKB_TRANSPORT_MAX_LEN,
              "canonical transport frame encoded") ||
      !expect(transport[transport_length - 1U] == 0U,
              "transport delimiter")) {
    return 1;
  }
  printf("transport_length=%zu\n", transport_length);
  printf("transport_hex=");
  for (size_t index = 0; index < transport_length; ++index) {
    printf("%02x", transport[index]);
  }
  printf("\n");
  return 0;
}
