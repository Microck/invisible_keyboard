#include "identity.h"

#include <string.h>

typedef struct {
  uint32_t state[8];
  uint64_t bit_length;
  uint8_t block[64];
  size_t block_length;
} sha256_context_t;

static const uint32_t SHA256_CONSTANTS[64] = {
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U, 0x3956c25bU,
    0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U, 0xd807aa98U, 0x12835b01U,
    0x243185beU, 0x550c7dc3U, 0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U,
    0xc19bf174U, 0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU, 0x983e5152U,
    0xa831c66dU, 0xb00327c8U, 0xbf597fc7U, 0xc6e00bf3U, 0xd5a79147U,
    0x06ca6351U, 0x14292967U, 0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU,
    0x53380d13U, 0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U, 0xd192e819U,
    0xd6990624U, 0xf40e3585U, 0x106aa070U, 0x19a4c116U, 0x1e376c08U,
    0x2748774cU, 0x34b0bcb5U, 0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU,
    0x682e6ff3U, 0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};

static uint32_t rotate_right(uint32_t value, uint32_t count) {
  return (value >> count) | (value << (32U - count));
}

static void sha256_transform(sha256_context_t *context) {
  uint32_t words[64];
  for (size_t index = 0U; index < 16U; ++index) {
    const size_t offset = index * 4U;
    words[index] = ((uint32_t)context->block[offset] << 24U) |
                   ((uint32_t)context->block[offset + 1U] << 16U) |
                   ((uint32_t)context->block[offset + 2U] << 8U) |
                   (uint32_t)context->block[offset + 3U];
  }
  for (size_t index = 16U; index < 64U; ++index) {
    const uint32_t s0 = rotate_right(words[index - 15U], 7U) ^
                        rotate_right(words[index - 15U], 18U) ^
                        (words[index - 15U] >> 3U);
    const uint32_t s1 = rotate_right(words[index - 2U], 17U) ^
                        rotate_right(words[index - 2U], 19U) ^
                        (words[index - 2U] >> 10U);
    words[index] = words[index - 16U] + s0 + words[index - 7U] + s1;
  }

  uint32_t a = context->state[0];
  uint32_t b = context->state[1];
  uint32_t c = context->state[2];
  uint32_t d = context->state[3];
  uint32_t e = context->state[4];
  uint32_t f = context->state[5];
  uint32_t g = context->state[6];
  uint32_t h = context->state[7];
  for (size_t index = 0U; index < 64U; ++index) {
    const uint32_t sum1 = rotate_right(e, 6U) ^ rotate_right(e, 11U) ^
                          rotate_right(e, 25U);
    const uint32_t choose = (e & f) ^ (~e & g);
    const uint32_t temporary1 =
        h + sum1 + choose + SHA256_CONSTANTS[index] + words[index];
    const uint32_t sum0 = rotate_right(a, 2U) ^ rotate_right(a, 13U) ^
                          rotate_right(a, 22U);
    const uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
    const uint32_t temporary2 = sum0 + majority;
    h = g;
    g = f;
    f = e;
    e = d + temporary1;
    d = c;
    c = b;
    b = a;
    a = temporary1 + temporary2;
  }

  context->state[0] += a;
  context->state[1] += b;
  context->state[2] += c;
  context->state[3] += d;
  context->state[4] += e;
  context->state[5] += f;
  context->state[6] += g;
  context->state[7] += h;
}

static void sha256_initialize(sha256_context_t *context) {
  static const uint32_t initial_state[8] = {
      0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
      0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
  };
  memset(context, 0, sizeof(*context));
  memcpy(context->state, initial_state, sizeof(initial_state));
}

static void sha256_update(sha256_context_t *context, const uint8_t *bytes,
                          size_t length) {
  for (size_t index = 0U; index < length; ++index) {
    context->block[context->block_length++] = bytes[index];
    if (context->block_length == sizeof(context->block)) {
      sha256_transform(context);
      context->bit_length += 512U;
      context->block_length = 0U;
    }
  }
}

static void sha256_finish(sha256_context_t *context, uint8_t digest[32]) {
  context->bit_length += (uint64_t)context->block_length * 8U;
  context->block[context->block_length++] = 0x80U;
  if (context->block_length > 56U) {
    while (context->block_length < sizeof(context->block)) {
      context->block[context->block_length++] = 0U;
    }
    sha256_transform(context);
    context->block_length = 0U;
  }
  while (context->block_length < 56U) {
    context->block[context->block_length++] = 0U;
  }
  for (size_t index = 0U; index < 8U; ++index) {
    context->block[63U - index] =
        (uint8_t)(context->bit_length >> (index * 8U));
  }
  sha256_transform(context);

  for (size_t word = 0U; word < 8U; ++word) {
    for (size_t byte = 0U; byte < 4U; ++byte) {
      digest[word * 4U + byte] =
          (uint8_t)(context->state[word] >> ((3U - byte) * 8U));
    }
  }
}

bool ikb_derive_controller_id(
    const uint8_t *provisioned_id, size_t provisioned_id_length,
    uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN]) {
  if (provisioned_id == NULL || provisioned_id_length == 0U ||
      controller_id_sha256 == NULL ||
      !ikb_has_nonzero_byte(provisioned_id, provisioned_id_length)) {
    return false;
  }

  sha256_context_t context;
  sha256_initialize(&context);
  sha256_update(&context, provisioned_id, provisioned_id_length);
  sha256_finish(&context, controller_id_sha256);
  return true;
}

bool ikb_create_boot_id(void *context, ikb_random_fill_fn random_fill,
                        uint8_t boot_id[IKB_BOOT_ID_LEN]) {
  if (random_fill == NULL || boot_id == NULL) {
    return false;
  }
  for (unsigned int attempt = 0U; attempt < 2U; ++attempt) {
    if (!random_fill(context, boot_id, IKB_BOOT_ID_LEN)) {
      memset(boot_id, 0, IKB_BOOT_ID_LEN);
      return false;
    }
    if (ikb_has_nonzero_byte(boot_id, IKB_BOOT_ID_LEN)) {
      return true;
    }
  }
  memset(boot_id, 0, IKB_BOOT_ID_LEN);
  return false;
}
