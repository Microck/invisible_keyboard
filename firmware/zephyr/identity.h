#ifndef INVISIBLE_KEYBOARD_ZEPHYR_IDENTITY_H_
#define INVISIBLE_KEYBOARD_ZEPHYR_IDENTITY_H_

#include "firmware/frame-protocol.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef bool (*ikb_random_fill_fn)(void *context, uint8_t *output,
                                   size_t output_length);

/* SHA-256 of the exact UTF-8 controller_id stored in the host manifest. */
bool ikb_derive_controller_id(
    const uint8_t *provisioned_id, size_t provisioned_id_length,
    uint8_t controller_id_sha256[IKB_CONTROLLER_ID_SHA256_LEN]);

/* Create a nonzero, per-boot identity from a cryptographic random source. */
bool ikb_create_boot_id(void *context, ikb_random_fill_fn random_fill,
                        uint8_t boot_id[IKB_BOOT_ID_LEN]);

#endif
