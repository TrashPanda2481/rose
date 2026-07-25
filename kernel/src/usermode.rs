// Ring 3 / user mode, v0.1.
//
// First code in the kernel that ever runs outside CPL0. Everything here
// is scaffolding for exactly one self-test: enter ring 3, prove two
// round-trip syscalls work (ring3 -> #GP-free int 0x80 -> ring0 handler
// -> back), then halt. No process model, no context saved to return to
// the interrupted kernel flow, no second user task. Those all need an
// AddressSpace/task abstraction that doesn't exist yet (see
// docs/cores/kernel/README.md, capability model, still spec only).
//
// enter_user_mode is one-way in this cut: iretq into ring 3 and there's
// nothing that ever iretqs back into whatever called it. on_syscall's
// second call halts the CPU directly instead of returning control to
// kernel_main. This is a deliberate, documented scope cut, not an
// oversight, see BUGS.md/README.md.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::gdt;
use crate::serial;

// Hand-written user-mode machine code, not compiled Rust: there's no
// user-mode runtime (no allocator, no panic handler, nothing) for a
// normal fn to run against yet, and a naked fn embedded in the kernel's
// own text section would still be mapped with kernel permissions, not
// USER. A dedicated asm blob with its own start/end labels is small
// enough to hand-place into a USER-mapped page via a plain memcpy,
// same linker-symbol convention paging.rs already uses for kernel
// section bounds.
core::arch::global_asm!(
    ".section .rodata.user_program, \"a\"",
    ".global user_program_start",
    ".global user_program_end",
    "user_program_start:",
    "mov rax, 0xabcd1234",
    "int 0x80",
    "mov rax, 1",
    "int 0x80",
    "2:",
    "jmp 2b",
    "user_program_end:",
    ".previous",
);

unsafe extern "C" {
    static user_program_start: u8;
    static user_program_end: u8;
}

/// Byte length of the embedded program, for the caller to copy exactly
/// that many bytes into the USER-mapped code page.
pub fn program_bytes() -> &'static [u8] {
    unsafe {
        let start = core::ptr::addr_of!(user_program_start) as *const u8;
        let end = core::ptr::addr_of!(user_program_end) as *const u8;
        let len = end as usize - start as usize;
        core::slice::from_raw_parts(start, len)
    }
}

static SYSCALL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Called from idt.rs's rose_exception_handler on every `int 0x80`.
/// Logs the syscall and, on the second one, halts for good: there's no
/// mechanism yet to resume the kernel flow that called enter_user_mode,
/// so this is the kernel's actual last statement in this v0.1 cut, not
/// a bug. See module docs above and BUGS.md.
pub fn on_syscall(rax: u64) {
    let mut com1 = serial::Serial::init();
    let count = SYSCALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let _ = writeln!(
        com1,
        "rose: usermode self-test: syscall #{} from ring 3, rax={:#x}",
        count, rax
    );

    if count >= 2 {
        let _ = writeln!(
            com1,
            "rose: usermode self-test: two round-trip syscalls confirmed, halting"
        );
        loop {
            unsafe { core::arch::asm!("hlt") };
        }
    }
}

/// Drops from ring 0 to ring 3 via iretq and never returns: there is no
/// caller to return to in this v0.1 cut (see module docs), and even if
/// there were, nothing currently arranges a way back short of another
/// interrupt. `entry`/`user_stack_top` must already be mapped USER
/// (code: R+X+U; stack: R+W+U) in the currently active address space,
/// and `gdt::set_kernel_stack` must already have been called so the
/// first ring3->ring0 trap (the first `int 0x80`) has a valid rsp0 to
/// land on.
///
/// Safety: caller must guarantee both of the above; getting either
/// wrong turns the very first instruction fetch or the very first trap
/// into a fault with no working stack to report it from.
pub unsafe fn enter_user_mode(entry: u64, user_stack_top: u64) -> ! {
    // Push order is SS, RSP, RFLAGS, CS, RIP so RIP ends up topmost
    // (last pushed = lowest address = first popped), matching iretq's
    // pop order and idt.rs's InterruptFrame field order (rip, cs,
    // rflags, rsp, ss from low to high address).
    core::arch::asm!(
        "push {ss}",
        "push {rsp}",
        "push {rflags}",
        "push {cs}",
        "push {rip}",
        "iretq",
        ss = in(reg) gdt::USER_DATA_SELECTOR as u64,
        rsp = in(reg) user_stack_top,
        // IF=1 (bit 1) plus the always-set reserved bit 1: ring 3 code
        // still needs interrupts enabled so a timer tick or the
        // syscall trap itself can be serviced normally.
        rflags = in(reg) 0x202u64,
        cs = in(reg) gdt::USER_CODE_SELECTOR as u64,
        rip = in(reg) entry,
        options(noreturn),
    );
}
