# Project notice

The canonical invisible-keyboard implementation is private work created under
the project's independent-implementation and no-artifact-reuse policy. It is
covered by the repository [license](LICENSE), which grants no reuse rights.

The ignored `research/` directory contains reference material under its own
licenses. In particular, LapCal's [pinned root
README](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org)
states CC BY-NC-SA 4.0 terms for its project. Project policy excludes importing
LapCal code, PCB files, CAD, datasets, calibration values, or model weights
into the canonical source, tests, protocol, layout, and implementation docs.
This notice does not claim strict clean-room separation or legal immunity.

Third-party Bazel modules and Rust crates retain their upstream licenses. Their
resolved versions are recorded in `MODULE.bazel` and `MODULE.bazel.lock`.
Redistribution must review and preserve the notices required by those upstream
licenses.

No third-party dataset, trained model, CAD file, or generated hardware design
is included in the canonical implementation at this stage.
