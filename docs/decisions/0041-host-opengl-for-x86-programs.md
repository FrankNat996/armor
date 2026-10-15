# 0041 — OpenGL of x86 Linux programs through the host's libGL

- **Status:** Proposed
- **Date:** 2026-10-15
- Context: M8 (`roadmap.md`: real graphics for x86 Linux programs, through calls to the host's own libraries), ADR-0018 (guest memory mapped at the same addresses as armor's), ADR-0008 (dependencies), `tech/steam.md`.

## Context
An x86 Linux game under armor draws in software: Debian's x86 Mesa has no driver for the Pi's GPU (`v3d: driver missing`), so llvmpipe renders on the translated CPU. Sound already works (the x86 PulseAudio client talks to the host's PipeWire). The host has the GPU's driver in its own (AArch64) Mesa.

Measured on Doki Doki Literature Club (Ren'Py, 2026-10-15): SDL2 creates the window with the game's x86 `libX11` and the OpenGL context with GLX, loading `libGL.so.1` with `dlopen`; Ren'Py's GL modules import 34 OpenGL 1.x functions directly (`glTexImage2D`, `glDrawArrays`, `glReadPixels`…) and 26 through GLEW, whose `glewInit` asks `glXGetProcAddressARB` for every function it knows.

## Decision
1. **The layer is libGL (GLX and OpenGL), not X11 or SDL.** The guest keeps its own `libX11` and its connection to the X server; X resources (windows, visuals, colormaps) are the server's, with the same ids for every client. armor's host side opens its own connection to the same display, and GLX calls are made there with the guest's ids (the window the guest created is drawable by the host's context). Structures GLX returns (`XVisualInfo`, `GLXFBConfig` arrays) are copied into guest memory, ids unchanged.
2. **The mechanism:** an x86-64 `libGL.so.1` of stubs, built from source in this repository with the x86-64 cross compiler, put in the guest root in place of Mesa's when the feature is on. Each stub stores its arguments (integers, floats and pointers alike) in a small structure in guest memory and makes a syscall with a number reserved for armor; armor reads the structure, calls the host function (found with `dlopen`/`dlsym` in the host's `libGL.so.1`, from libc: no new dependency) on the guest thread's own host thread (GL contexts are per thread), and writes the result back. Guest pointers are host pointers (ADR-0018), so arrays and buffers pass as they are.
3. **What crosses back is copied or registered:** strings the host returns (`glGetString`, `glXQueryExtensionsString`) are copied into guest memory once; memory the driver maps for the guest (`glMapBuffer`, `glMapBufferRange`) is added to armor's page tables for its lifetime; `glXGetProcAddress` returns the guest stub of the function, never a host address, and NULL for one without a stub.
4. **Which functions:** the stubs and armor's table are generated from Khronos' `gl.xml` and `glx.xml` (pinned by version and SHA-256), for the desktop OpenGL core and compatibility profiles and the extensions Mesa's v3d exposes; a function whose signature the generator cannot handle (callbacks: `glDebugMessageCallback`) is left out and reported. Increment 1 is one hand-written function through the whole path, with its test in `tests/guest/`, before the generator.
5. **Verification:** `difftest` cannot compare a GPU with qemu's software renderer pixel by pixel, so: test programs that draw scenes both rasterizers must agree on exactly (axis-aligned rectangles of flat colours, clears, scissors), read back with `glReadPixels` and compared with the same program under qemu (llvmpipe); the generator's output checked against the XML (as `encoder-check` checks the encoder); and the real case: the game's own log reporting the host's renderer (`V3D`) and drawing its scenes.
6. **Opt-in at first:** a `--host-gl` option and `host-gl = yes` in profiles and `app.conf`, until the games tried show it is safe as the default.

## Alternatives considered
- **An x86-64 Mesa built with the Pi's v3d driver**, the driver's own ioctls passed through armor: no translation layer to write, but the driver's CPU work stays translated (slower), it covers only the GPUs whose drivers are built, and it needs a Mesa build for x86 kept in step with the host's kernel. Not chosen.
- **Calling the host's libX11 too (as FEX's thunks do):** the most general, but X11 has hundreds of functions and callbacks; GLX with shared X ids avoids it for OpenGL.
- **Calling the host's SDL2:** covers window, input and GL at once, but SDL is not the only way games open a window (GLFW, raw X11), and its callbacks (audio) would call guest code from host threads.

## Consequences
- \+ x86 games draw with the host's GPU driver, and the driver's own work runs natively.
- \+ The same path serves every ARM64 GPU with a Mesa driver (M9).
- − A generated layer of a few thousand functions to keep correct; callbacks into guest code are not covered at first.
- − Vulkan (DXVK in Proton is not concerned: Proton is native) and EGL are later steps, on the same mechanism.
