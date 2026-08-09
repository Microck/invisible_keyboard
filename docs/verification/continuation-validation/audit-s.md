I read the bundle's `SHA256SUMS` and compared it to the guest artifact receipt, and every listed identity matches. That closes my remaining caveat, so my final verdict is COMPLETE. The only work left is physical.

**Matches between the bundle's `SHA256SUMS` and `x86/artifacts.sha256`**
- **x86 CLI:** `fddd7d22…0878` matches the guest `bazel-bin/invisible-keyboard`.
- **Left firmware:** the elf (`5aaa…c3a`), hex (`349a…81e`) and uf2 (`8c31…34a`) all match.
- **Right firmware:** the elf (`4aca…e9a`), hex (`618c…f3f`) and uf2 (`4786…831`) all match.
- **Firmware provenance:** `446b…258` matches.
- **Capture stack:**
  - `sigrok-cli-gate0a`: `1665…5c3`
  - DSLogic firmware tar: `bc85…8de`
  - `libsigrok` patch: `55d3…10d`
  - `BUILD-INFO`: `0fcb…268`
  - provenance file: `2699…86d`

  The patch and provenance file appear twice in the bundle, once under the artifacts and once under the corresponding source, and both copies carry the same hash.
- **Source manifest:** `source-manifest.sha256` is listed as `5b6c2109…478e6`, the frozen v13 identity.
- **Other bundle contents with nothing to compare against:** the corresponding-source tars, license texts, build logs and `reference-source.tar.gz` (`1af6…15f`) are listed but appear in no other receipt I read. They are bound only by this list and the sealed receipt manifest.
- **Native CLI:** it is kept outside the bundle and bound separately at `9b57…0518` in `native/artifacts.sha256` and `durability-binding.json`.

**Remaining limit:** every digest was computed in Crabbox. I checked them against each other by reading and did not recompute any. That includes the bundle file contents, the archive itself and the four-manifest equality, which rests on the Crabbox collector.

**Physical-only gates**
These are hardware work, not unfinished software:
- received-part, harness, electrical and thermal qualification
- flashing and readback
- register, interrupt, I2C, USB, analyzer and quiesce timing
- real axis poses and the Gate 0A soak
- the human pilot
- contact and finger observability, and separability of the 34 positions
- remount, surface and person transfer
- false activation during ordinary motion
- real host focus, IME, suspend and disconnect behaviour
- typing speed, error rate, comfort and safety

**Verdict:** standards are met, the spec is met, I have no material source findings, and all required native and guest lanes are proven. The completion audit is clean, with about 0.82 confidence.
