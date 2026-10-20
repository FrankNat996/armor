# 0048 — Translations cached on disk

- **Status:** Proposed
- **Date:** 2026-10-20

## Context
Item 6 of M11 asks for a lower translation cost. On Python's startup (`cargo xtask bench`, `python-start`, Pi 5 with 4K pages, 2026-10-20) armor takes 183 ms, of which 92 ms translating 16,779 blocks (243,864 x86 instructions, about 375 ns each); box64 takes 104 ms, natively 15 ms. Translation also costs 101 ms of `python-int`'s 691 ms and 8 of `start-dynamic`'s 18. What was tried (`tech/performance.md`):
- interpreting blocks a few times before translating them (2026-10-20): 3% at best, the interpreter is too slow;
- cheaper translation (vectors with capacity, constants without allocation, shared exits, shared prologue and epilogue): 123 → 92 ms; the time is spread over the translator, the decoder and the allocator, with no part above a fifth of it.

Every run of a program translates the same code again: the libraries and the program are the same files, mapped at the same addresses (7-Zip's two runs of 2026-10-20 under `ARMOR_TRACE_MAPS`, with different environments, mapped the program and its libraries at the same addresses; the key below does not rely on it), and the translator is a pure function of the block's bytes, its address and the options (`Options`). Since 2026-10-20 a translated block no longer depends on where it is published: the prologue, the epilogue and the flag routine are shared and reached through the `Context`, exits are relative branches or table lookups, the fault table holds offsets.

## Decision
armor keeps the translations of file-backed code in a cache on disk and reuses them in later runs:
1. **What:** for each executable mapping of a file, the blocks translated in it: the block's guest address (relative to the mapping), a hash of its guest bytes, its words as translated (exits not yet chained), its fault table and its entry kind. Only blocks of file-backed, unmodified mappings (not anonymous or written code, not JIT output of the guest).
2. **Key:** the file's identity (device, inode, size, modification time, and a hash of the mapped range), the mapping's guest address, the translation options (`Options`, the host features) and armor's own build (a hash of its executable). Any difference is a miss; the cache never decides that two translations are equivalent.
3. **Use:** at a block's first entry, the engine looks for it in the mapping's cache file before translating; a hit is checked against the guest bytes now in memory (the hash), then published as a fresh translation. New translations are appended to the cache when the process exits or the cache file is written (atomic rename, so concurrent armor processes never see half a file).
4. **Where:** `$XDG_CACHE_HOME/armor/jit/` (else `~/.cache/armor/jit/`), files mode 0600, at most a configurable size (oldest dropped). `ARMOR_JIT_CACHE=off` disables it; `--sandbox` (ADR-0027) never uses it, as the sandboxed guest is untrusted and the cache holds host code.
5. **Verification:** `difftest` and lockstep with the cache cold, warm and corrupted (a flipped byte must be a miss, not a crash); `bench` reports cold and warm runs separately, so that the matrix does not hide the cold cost.

## Alternatives considered
- **A faster translator only:** worthwhile anyway, but the measured costs are spread out; halving 375 ns per instruction is months of work and would still leave Python's startup above box64's.
- **Translating less** (only code that runs often): the interpreter is 20 to 50 times slower than translated code, so cold code interpreted costs more than its translation (measured).
- **Translating in a background thread:** hides latency only where the main thread can run something else; at startup it waits for the code it needs.

## Consequences
- \+ Second and later starts of a program skip most translation: Python's startup would fall from 183 ms towards 90 ms, below box64's 104.
- \+ A cache shared by every program that uses the same libraries (libc, libstdc++).
- − Disk space, and a file format to keep valid across armor versions (the build hash makes old caches misses).
- − Host code read from disk: a process of the same user that can write the cache can make armor run code of its choice. It could equally replace armor's executable; the sandbox does not use the cache.
- − First runs are as slow as now; the matrix must say which runs were warm.
