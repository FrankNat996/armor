//! The pieces of an ARM64EC image that MSVC's C runtime would bring, and
//! the backend's entry and exit points, in assembly.
//!
//! **The image.** Wine's loader accepts a DLL as ARM64EC when its load
//! configuration (`_load_config_used`) points to CHPE metadata
//! (`__chpe_metadata`): the code map, the x64 entry points and the
//! redirections, which `lld-link` makes (`__hybrid_code_map`, …), and the
//! addresses of the dispatch pointers the loader fills
//! (`__os_arm64x_dispatch_call_no_redirect`, `__os_arm64x_check_icall`,
//! …), which code generated for ARM64EC calls through: exit thunks to x64
//! code, entry thunks back, indirect calls.
//!
//! **The emulator's interface** (Wine's `dlls/ntdll/signal_arm64ec.c`):
//! ARM64EC code reaches x86-64 code through four exported addresses,
//! with the x86-64 state in the registers of ARM64EC's mapping (rax = x8,
//! rcx = x0, rdx = x1, rbx = x27, rsp = sp, rbp = x29, rsi = x25, rdi =
//! x26, r8–r11 = x2–x5, r12–r15 = x19–x22, xmm0–15 = v0–v15) and the
//! target in x9:
//! - `ExitToX64` (`__os_arm64x_dispatch_call_no_redirect`): a call from an
//!   exit thunk (`blr x16`): the return address is pushed, as x86-64's
//!   `call` would;
//! - `DispatchJump` (`__os_arm64x_x64_jump`): the same, for a target that
//!   may be ARM64EC code after all;
//! - `RetToEntryThunk` (`__os_arm64x_dispatch_ret`): an entry thunk
//!   returns to its x86-64 caller, whose address is in lr;
//! - `BeginSimulation`: Wine resumes x86-64 code from the thread's
//!   x86-64 CONTEXT (thread start, `NtContinue`, exceptions).
//!
//! The backend saves the registers into the thread's state, runs the guest
//! on its own stack (`armor_dispatch`) until the guest reaches ARM64EC code
//! (the PEB's bitmap), loads the registers back and goes there: straight
//! to a return address after an exit thunk's `blr x16`, otherwise through
//! the function's entry thunk (its offset in the word before the
//! function), with the function in x9, the x86-64 stack arguments at x4,
//! and the x86-64 return address in lr when the stack shows one (popped:
//! the entry thunk returns through `__os_arm64x_dispatch_ret`), as
//! Microsoft's emulator does.

core::arch::global_asm!(
    r##"
    // CHPE metadata, version 1 (the layout `llvm-readobj --coff-load-config`
    // reads).
    .section .rdata,"dr"
    .p2align 2
    .globl __chpe_metadata
__chpe_metadata:
    .word 1
    .rva __hybrid_code_map
    .word __hybrid_code_map_count
    .rva __x64_code_ranges_to_entry_points
    .rva __arm64x_redirection_metadata
    .rva __os_arm64x_dispatch_call_no_redirect
    .rva __os_arm64x_dispatch_ret
    .rva __os_arm64x_check_call
    .rva __os_arm64x_check_icall
    .rva __os_arm64x_check_icall_cfg
    .rva __arm64x_native_entrypoint
    .rva __hybrid_auxiliary_iat
    .word __x64_code_ranges_to_entry_points_count
    .word __arm64x_redirection_metadata_count
    .rva __os_arm64x_get_x64_information
    .rva __os_arm64x_set_x64_information
    .rva __arm64x_extra_rfe_table
    .word __arm64x_extra_rfe_table_size
    .rva __os_arm64x_dispatch_fptr
    .rva __hybrid_auxiliary_iat_copy

    // IMAGE_LOAD_CONFIG_DIRECTORY64: its size, and the CHPE metadata at
    // 0xc8; nothing else is used.
    .p2align 3
    .globl _load_config_used
_load_config_used:
    .word 0x138
    .fill 0xc4, 1, 0
    .xword __chpe_metadata
    .fill 0x68, 1, 0

    // The pointers the loader fills.
    .data
    .p2align 3
    .globl __os_arm64x_dispatch_call_no_redirect
__os_arm64x_dispatch_call_no_redirect:
    .xword 0
    .globl __os_arm64x_dispatch_ret
__os_arm64x_dispatch_ret:
    .xword 0
    .globl __os_arm64x_check_call
    .globl __os_arm64x_dispatch_call
__os_arm64x_check_call:
__os_arm64x_dispatch_call:
    .xword 0
    .globl __os_arm64x_check_icall
    .globl __os_arm64x_dispatch_icall
__os_arm64x_check_icall:
__os_arm64x_dispatch_icall:
    .xword 0
    .globl __os_arm64x_check_icall_cfg
    .globl __os_arm64x_dispatch_icall_cfg
__os_arm64x_check_icall_cfg:
__os_arm64x_dispatch_icall_cfg:
    .xword 0
    .globl __os_arm64x_get_x64_information
__os_arm64x_get_x64_information:
    .xword 0
    .globl __os_arm64x_set_x64_information
__os_arm64x_set_x64_information:
    .xword 0
    .globl __os_arm64x_dispatch_fptr
    .globl __os_arm64x_x64_jump
__os_arm64x_dispatch_fptr:
__os_arm64x_x64_jump:
    .xword 0

    // EmulatorData[2]'s value, for the thread's setup.
    .globl ARMOR_ENTER_EC
ARMOR_ENTER_EC:
    .xword armor_enter_ec

    .text
    .p2align 4

    // The linker's import thunks end here (the target in x11, its exit
    // thunk in x10), before the loader fills the auxiliary import table:
    // the call checker decides where to go.
    .globl __icall_helper_arm64ec
__icall_helper_arm64ec:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    adrp x16, __os_arm64x_check_icall
    ldr x16, [x16, :lo12:__os_arm64x_check_icall]
    blr x16
    ldp x29, x30, [sp], #16
    br x11

    // From ARM64EC code to a target that may be either kind (x9).
    .globl DispatchJump
DispatchJump:
    str x30, [sp, #-8]!
    b armor_check_target

    // An entry thunk returns to x86-64 code, or to ARM64EC code (lr).
    .globl RetToEntryThunk
RetToEntryThunk:
    mov x9, x30
armor_check_target:
    ldr x16, [x18, #0x60]
    ldr x16, [x16, #0x368]
    lsr x17, x9, #18
    ldr x16, [x16, x17, lsl #3]
    lsr x17, x9, #12
    lsr x16, x16, x17
    tbnz x16, #0, armor_to_ec
    b armor_enter

    // From an exit thunk to x86-64 code (x9).
    .globl ExitToX64
ExitToX64:
    str x30, [sp, #-8]!
armor_enter:
    ldr x17, [x18, #0x1788]
    mov w16, #1
    strb w16, [x17]
    ldr x16, [x17, #0x40]
    br x16

    // EmulatorData[2]: the x86-64 state in the registers, rip in x9, the
    // CPU area in x17.
armor_enter_ec:
    ldr x16, [x17, #0x30]
    stp x8, x0, [x16, #0]
    stp x1, x27, [x16, #16]
    mov x0, sp
    stp x0, x29, [x16, #32]
    stp x25, x26, [x16, #48]
    stp x2, x3, [x16, #64]
    stp x4, x5, [x16, #80]
    stp x19, x20, [x16, #96]
    stp x21, x22, [x16, #112]
    str x9, [x16, #128]
    add x0, x16, #144
    stp q0, q1, [x0], #32
    stp q2, q3, [x0], #32
    stp q4, q5, [x0], #32
    stp q6, q7, [x0], #32
    stp q8, q9, [x0], #32
    stp q10, q11, [x0], #32
    stp q12, q13, [x0], #32
    stp q14, q15, [x0], #32
    ldr x0, [x17, #8]
    mov sp, x0
    mov x0, x16
    mov w1, #0
    bl "#armor_dispatch"
    b armor_leave

    // Wine resumes x86-64 code from the thread's x86-64 CONTEXT.
    .globl BeginSimulation
BeginSimulation:
    ldr x17, [x18, #0x1788]
    ldr x16, [x17, #8]
    mov sp, x16
    ldr x0, [x17, #0x30]
    mov w1, #1
    bl "#armor_dispatch"

    // The guest reached ARM64EC code: its state from the thread's (x0)
    // into the registers, then there.
armor_leave:
    mov x16, x0
    add x17, x16, #144
    ldp q0, q1, [x17], #32
    ldp q2, q3, [x17], #32
    ldp q4, q5, [x17], #32
    ldp q6, q7, [x17], #32
    ldp q8, q9, [x17], #32
    ldp q10, q11, [x17], #32
    ldp q12, q13, [x17], #32
    ldp q14, q15, [x17], #32
    ldp x8, x0, [x16, #0]
    ldp x1, x27, [x16, #16]
    ldp x17, x29, [x16, #32]
    mov sp, x17
    ldp x25, x26, [x16, #48]
    ldp x2, x3, [x16, #64]
    ldp x4, x5, [x16, #80]
    ldp x19, x20, [x16, #96]
    ldp x21, x22, [x16, #112]
    ldr x9, [x16, #128]

    // To ARM64EC code at x9, out of the simulation.
armor_to_ec:
    ldr x17, [x18, #0x1788]
    strb wzr, [x17]
    ldursw x10, [x9, #-4]
    mov w16, #0x0200
    movk w16, #0xd63f, lsl #16
    cmp w10, w16
    b.eq 1f
    and x10, x10, #-4
    add x16, x9, x10
    // The entry thunk calls x9 with the x86-64 stack arguments at x4
    // (r10, which a call does not preserve).
    mov x4, sp
    tbz x4, #3, 2f
    ldr x30, [x4], #8
    mov sp, x4
    br x16
2:
    adrp x30, ARMOR_X64_RET
    ldr x30, [x30, :lo12:ARMOR_X64_RET]
    br x16
1:
    br x9

    // The four entry points are exported as addresses, not as functions
    // (no x64 export thunks).
    .section .drectve,"yn"
    .ascii " /EXPORT:DispatchJump,DATA /EXPORT:RetToEntryThunk,DATA"
    .ascii " /EXPORT:ExitToX64,DATA /EXPORT:BeginSimulation,DATA"
"##
);

unsafe extern "C" {
    /// The address of the entry from ARM64EC code (EmulatorData[2]).
    pub static ARMOR_ENTER_EC: u64;
}
