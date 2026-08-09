# Gate 0A capture-stack provenance and license notice

This repository does not vendor an upstream binary. The Box-only build recipe
checks out exact upstream commits, applies the project patch, builds a
`sigrok-cli` executable with the patched `libsigrok` linked into it, and emits
the sources and notices needed to reproduce that executable.

## Pinned upstream source

| Component | Repository and revision | License evidence |
|---|---|---|
| libsigrok | [sigrokproject/libsigrok](https://github.com/sigrokproject/libsigrok/tree/0bc2487778e660f4d3116729b6f4aee2b1996bb0), commit `0bc2487778e660f4d3116729b6f4aee2b1996bb0` | Upstream `COPYING`; the changed DSLogic files state GPL-3.0-or-later |
| sigrok-cli | [sigrokproject/sigrok-cli](https://github.com/sigrokproject/sigrok-cli/tree/f44dd91347e7ac797cefc23162b9fcf0b7329f1f), commit `f44dd91347e7ac797cefc23162b9fcf0b7329f1f` | Upstream `COPYING`; individual files state GPL-3.0-or-later |
| sigrok-firmware collection | [sigrokproject/sigrok-firmware](https://github.com/sigrokproject/sigrok-firmware/tree/11eed913a9c535b87b5d5b5b92d2622cf34cee8b), commit `11eed913a9c535b87b5d5b5b92d2622cf34cee8b` | Its `README` and `COPYING` say the collection has no project-wide license and each firmware file has its own terms |
| DSLogic firmware extraction recipe | [sigrokproject/sigrok-util](https://github.com/sigrokproject/sigrok-util/blob/69747ca48b3d17ac7532439e487b9ba829828cfd/firmware/dreamsourcelab-dslogic/sigrok-fwextract-dreamsourcelab-dslogic), commit `69747ca48b3d17ac7532439e487b9ba829828cfd` | The extraction script states GPL-3.0-or-later |
| DSLogic Plus firmware bytes | [DreamSourceLab/DSView](https://github.com/DreamSourceLab/DSView/tree/886b847c21c606df3138ce7ad8f8e8c363ee758b), commit `886b847c21c606df3138ce7ad8f8e8c363ee758b` | Upstream `COPYING` is GPL-3.0 |

The pinned `sigrok-firmware` collection does **not** contain the DSLogic Plus
FX2 firmware or FPGA bitstream. The canonical bundle therefore follows the
pinned sigrok-util extraction recipe and takes the two files from the exact
DSView commit that recipe names. The bundle includes the DSView license text.

| Installed name | Source path | Bytes | SHA-256 |
|---|---|---:|---|
| `dreamsourcelab-dslogic-plus-fpga.fw` | `DSView/res/DSLogicPlus.bin` | 341,160 | `e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7` |
| `dreamsourcelab-dslogic-plus-fx2.fw` | `DSView/res/DSLogicPlus.fw` | 8,120 | `9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e` |

The open firmware and gateware source repositories are
[DreamSourceLab/DSLogic-fw](https://github.com/DreamSourceLab/DSLogic-fw) and
[DreamSourceLab/DSLogic-hdl](https://github.com/DreamSourceLab/DSLogic-hdl).
The build recipe does not claim that a reviewed, reproducible build connects
those repositories to the two historical DSView blobs. Byte identity is
fixed. Source-to-binary reproducibility remains a firmware provenance risk.

## Project patch

`libsigrok-0bc2487-dslogic-gate0a.patch` changes only the pinned DSLogic
driver. It:

- changes the long capture counter from `unsigned int` to `uint64_t`;
- rejects every non-completed USB status, empty transfer, incomplete DSLogic
  sample block, and failed resubmission;
- counts each validated emitted USB block once in a `uint64_t`;
- emits the internal receipt
  `IKB_DSLOGIC_TRANSFER_V1 completed_transfers=N anomalies=0` only after the
  exact requested sample limit has been emitted; and
- preserves the original file notices and GPL-3.0-or-later terms.

The patch is a derivative modification of the named GPL files and is offered
under GPL-3.0-or-later. It has no warranty.

## Distribution obligations

When distributing the generated capture executable, distribute the generated
`corresponding-source/` directory with it. It contains the exact upstream
source archives, this patch, the build recipe, provenance, and license texts.
Retain copyright notices. Provide recipients the same GPL rights to copy,
inspect, rebuild, and modify the covered source.

The generated firmware tar contains the exact two firmware blobs, a manifest,
and the DSView GPL text. Before shipping hardware or redistributing those
blobs outside the development team, confirm that the available firmware and
HDL repositories satisfy the required corresponding-source obligation for
the historical binaries. This is a legal and supply-chain review item, not a
waveform-scoring exception.

## Physical trust boundary

The patch and hashes can prove which software and firmware bytes were used.
They cannot prove analyzer clock accuracy, probe continuity, voltage-threshold
margin, absence of analog glitches between 2 MHz samples, USB host stability,
or a loss-free 3,610-second physical run. Gate 0A still requires the physical
preflight, soak, and full capture described in the implementation guide.
