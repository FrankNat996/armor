# Khronos OpenGL registry (third party)

`gl.xml` and `glx.xml` from the Khronos Group's OpenGL registry
(https://github.com/KhronosGroup/OpenGL-Registry, directory `xml/`), commit
`6af574a14089ccfee87efe230ebcdd8742859813` (2026-10-06), unchanged:

| File | SHA-256 |
|---|---|
| `gl.xml` | `b9ca2cfa5c676e901c20d34af3407f1687cde0f1336a5ff7a8974d04c7494ad3` |
| `glx.xml` | `e977a1518ad70bde5fd25bf29941361cc7ef76836e2611d8ddcd625b3209a598` |

Copyright 2013-2026 The Khronos Group Inc., licensed under the Apache License 2.0
(`SPDX-License-Identifier: Apache-2.0`, in each file's header). armor's generator
(`cargo xtask gl-gen`, ADR-0041) reads them to produce the OpenGL stubs and the
host call table.
