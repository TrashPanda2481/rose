// Interrupt Descriptor Table, v0.1.
//
// Covers the 32 real CPU exception vectors plus vector 32 (IRQ0, PIT
// timer, see pic.rs/timer.rs) now that something actually unmasks and
// drives a hardware interrupt line. Vectors 33-255 are still left
// not-present on purpose: nothing else is unmasked at the PIC yet, so no
// other external interrupt can legitimately arrive. If one somehow did,
// a not-present gate turns it into a #GP, which we do handle, so it still
// gets logged instead of vanishing into an undefined CPU state.
//
// Vector 32's dispatch also drives the scheduler (scheduler::tick()),
// since a timeslice expiring is itself just a timer tick.
//
// Every stub is hand-written asm, not the unstable `x86-interrupt` Rust
// ABI. Slightly more code, but the stack layout is fully ours to see and
// name, nothing about it depends on how a future compiler happens to
// lower an experimental calling convention.
//
// Every CPU exception here is fatal except the breakpoint (#BP), which
// logs and returns; that's what the self-test in main.rs uses to prove
// the whole GDT/IDT/TSS chain works without having to crash the kernel
// to prove it. Vector 32 (the timer tick) is not an exception at all and
// is dispatched separately, before the exception-logging path: it just
// bumps the tick counter, sends EOI, and returns, every 10ms, silently.

use core::fmt::Write;

use crate::gdt::{DOUBLE_FAULT_IST_INDEX, KERNEL_CODE_SELECTOR};
use crate::mem::SpinLock;
use crate::pic;
use crate::scheduler;
use crate::serial;
use crate::syscall;
use crate::timer;
use crate::usermode;

/// Software interrupt vector ring 3 code uses to call into the kernel.
/// Arbitrary choice within the unused 33-255 range, 0x80 just matches
/// the long-standing convention from other syscall-via-int designs.
pub const SYSCALL_VECTOR: u8 = 0x80;

#[repr(C)]
struct SavedRegs {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
}

/// Layout matches exactly what the stubs below push, low address first.
/// `vector`/`error_code` are ours; `rip` through `ss` are pushed by the
/// CPU itself before the stub ever runs.
#[repr(C)]
struct InterruptFrame {
    regs: SavedRegs,
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

#[repr(C, packed)]
struct Entry {
    offset_low: u16,
    selector: u16,
    ist_and_reserved: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl Entry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist_and_reserved: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn set(&mut self, handler: u64, ist_index: u8) {
        self.offset_low = handler as u16;
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.selector = KERNEL_CODE_SELECTOR;
        self.ist_and_reserved = ist_index & 0x7;
        // Present=1, DPL=00, 64-bit interrupt gate (0xE). Interrupt gate,
        // not trap gate, so IF is cleared on entry: none of these handlers
        // are written to tolerate a nested interrupt firing mid-handler.
        self.type_attr = 0x8E;
    }

    /// Same as `set`, except DPL=11 instead of DPL=00. Used only for the
    /// syscall gate: a ring-3 `int 0x80` against a DPL=00 gate would take
    /// a #GP instead of entering the handler, since the CPU checks the
    /// gate's DPL against CPL for a software `int` (not for a hardware
    /// IRQ or CPU exception, which is why every other gate stays DPL=00).
    fn set_user(&mut self, handler: u64, ist_index: u8) {
        self.offset_low = handler as u16;
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.selector = KERNEL_CODE_SELECTOR;
        self.ist_and_reserved = ist_index & 0x7;
        // Present=1, DPL=11, 64-bit interrupt gate (0xE).
        self.type_attr = 0xEE;
    }
}

static IDT: SpinLock<[Entry; 256]> = SpinLock::new([Entry::missing(); 256]);

impl Clone for Entry {
    fn clone(&self) -> Self {
        Entry { ..*self }
    }
}
impl Copy for Entry {}

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

fn exception_name(vector: u64) -> &'static str {
    match vector {
        0 => "divide error (#DE)",
        1 => "debug (#DB)",
        2 => "non-maskable interrupt",
        3 => "breakpoint (#BP)",
        4 => "overflow (#OF)",
        5 => "bound range exceeded (#BR)",
        6 => "invalid opcode (#UD)",
        7 => "device not available (#NM)",
        8 => "double fault (#DF)",
        9 => "coprocessor segment overrun (legacy)",
        10 => "invalid TSS (#TS)",
        11 => "segment not present (#NP)",
        12 => "stack-segment fault (#SS)",
        13 => "general protection fault (#GP)",
        14 => "page fault (#PF)",
        16 => "x87 floating-point exception (#MF)",
        17 => "alignment check (#AC)",
        18 => "machine check (#MC)",
        19 => "SIMD floating-point exception (#XM)",
        20 => "virtualization exception (#VE)",
        21 => "control protection exception (#CP)",
        28 => "hypervisor injection exception (#HV)",
        29 => "VMM communication exception (#VC)",
        30 => "security exception (#SX)",
        _ => "reserved vector",
    }
}

fn read_cr2() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Common path for every exception. Breakpoint returns normally; every
/// other vector logs everything it can and halts. No recovery mechanism
/// exists yet (no scheduler, no per-thread fault policy), so "halt" is the
/// only honest option for anything that isn't the deliberate self-test.
#[unsafe(no_mangle)]
extern "C" fn rose_exception_handler(frame: *mut InterruptFrame) {
    // Mutable, not the original shared reference: writing a result
    // back into frame.regs.rax below is what lets a CSpace syscall
    // actually return a value to ring 3. Sound because the pointer
    // this came in as was already *mut InterruptFrame (common_stub
    // hands off `rsp` into a region it exclusively owns for the
    // duration of this call), so reborrowing it mutably here doesn't
    // create any aliasing that wasn't already possible.
    let frame = unsafe { &mut *frame };
    let vector = frame.vector;

    if vector == SYSCALL_VECTOR as u64 {
        // Software syscall, not a hardware tick or a fault. Same
        // reasoning as the IRQ0 branch just below for why this is
        // special-cased ahead of the generic exception-logging path:
        // this is an expected, frequent, non-error control transfer,
        // not something to run through the fatal-by-default handler.
        //
        // Three families share this one vector: the legacy sentinel
        // self-test (0xabcd1234, 1, predating any real ABI, no return
        // value ever used), the CSpace syscalls, and Retype. Routed by
        // number, not by anything structural, since all three arrive
        // the same way (int 0x80).
        if syscall::is_cspace_syscall(frame.regs.rax) {
            let num = frame.regs.rax;
            let arg1 = frame.regs.rdi;
            let arg2 = frame.regs.rsi;
            let arg3 = frame.regs.rdx;
            let arg4 = frame.regs.r10;
            let result = syscall::dispatch(num, arg1, arg2, arg3, arg4);
            usermode::on_cspace_syscall(num, arg1, arg2, arg3, arg4, result);
            // Written into the saved-register area common_stub will
            // pop rax from right before iretq, so this is what ring 3
            // actually sees land in its own rax after the int 0x80
            // returns.
            frame.regs.rax = result;
        } else if syscall::is_untyped_syscall(frame.regs.rax) {
            let arg1 = frame.regs.rdi;
            let arg2 = frame.regs.rsi;
            let arg3 = frame.regs.rdx;
            let result = syscall::dispatch_untyped(arg1, arg2, arg3);
            usermode::on_untyped_syscall(arg1, arg2, arg3, result);
            frame.regs.rax = result;
        } else if syscall::is_configure_syscall(frame.regs.rax) {
            let arg1 = frame.regs.rdi;
            let arg2 = frame.regs.rsi;
            let arg3 = frame.regs.rdx;
            let arg4 = frame.regs.r10;
            // Configure is the first syscall to need a 5th argument;
            // r8 has been sitting in SavedRegs/common_stub the whole
            // time (every branch above it just never had a reason to
            // read it out), so no calling-convention or stub change is
            // needed here, only reading one more field.
            let arg5 = frame.regs.r8;
            let result = syscall::dispatch_configure(arg1, arg2, arg3, arg4, arg5);
            usermode::on_configure_syscall(arg1, arg2, arg3, arg4, arg5, result);
            frame.regs.rax = result;
        } else if syscall::is_frame_map_syscall(frame.regs.rax) {
            let arg1 = frame.regs.rdi;
            let arg2 = frame.regs.rsi;
            let arg3 = frame.regs.rdx;
            let arg4 = frame.regs.r10;
            let result = syscall::dispatch_frame_map(arg1, arg2, arg3, arg4);
            usermode::on_frame_map_syscall(arg1, arg2, arg3, arg4, result);
            frame.regs.rax = result;
        } else {
            usermode::on_syscall(frame.regs.rax);
        }
        return;
    }

    if vector == pic::IRQ0_TIMER_VECTOR {
        // Not an exception; a hardware tick. No logging on the hot path,
        // every serial write here would mean 100 lines/sec forever.
        //
        // EOI has to go out before anything else runs, not after:
        // scheduler::tick() can context-switch away, and this exact call
        // frame then doesn't "return" (doesn't continue past this point)
        // until this task is rescheduled, possibly many ticks later. If
        // EOI hadn't been sent yet, the PIC's IRQ0 in-service bit would
        // stay set that whole time, blocking every further IRQ0; that
        // would mean no more ticks ever fire, so this task would never
        // get rescheduled either. EOI is unconditional and immediate,
        // decoupled from whatever the handler body does afterward.
        unsafe { pic::send_eoi(0) };
        timer::on_tick();
        scheduler::tick();
        return;
    }

    let mut com1 = serial::Serial::init();
    let rip = frame.rip;
    let error_code = frame.error_code;

    let _ = writeln!(
        com1,
        "rose: EXCEPTION vector={} ({}) error_code={:#x} rip={:#018x}",
        vector,
        exception_name(vector),
        error_code,
        rip
    );

    if vector == 14 {
        let _ = writeln!(com1, "rose:   faulting address (cr2)={:#018x}", read_cr2());
    }

    if vector == 3 {
        // Breakpoint: logged above, now just return. RIP already points
        // past the one-byte int3 opcode, the CPU advances it automatically
        // since this is a trap, not a fault.
        return;
    }

    let _ = writeln!(com1, "rose: unrecoverable, halting");
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

macro_rules! define_stub {
    ($name:ident, $vector:expr, no_error_code) => {
        #[unsafe(naked)]
        extern "C" fn $name() -> ! {
            core::arch::naked_asm!(
                "push 0",
                "push {vec}",
                "jmp {common}",
                vec = const $vector,
                common = sym common_stub,
            )
        }
    };
    ($name:ident, $vector:expr, has_error_code) => {
        #[unsafe(naked)]
        extern "C" fn $name() -> ! {
            core::arch::naked_asm!(
                "push {vec}",
                "jmp {common}",
                vec = const $vector,
                common = sym common_stub,
            )
        }
    };
}

#[unsafe(naked)]
extern "C" fn common_stub() -> ! {
    core::arch::naked_asm!(
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov rdi, rsp",
        "call {handler}",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        "add rsp, 16", // drop our vector + error_code slots
        "iretq",
        handler = sym rose_exception_handler,
    )
}

define_stub!(stub_00, 0, no_error_code);
define_stub!(stub_01, 1, no_error_code);
define_stub!(stub_02, 2, no_error_code);
define_stub!(stub_03, 3, no_error_code);
define_stub!(stub_04, 4, no_error_code);
define_stub!(stub_05, 5, no_error_code);
define_stub!(stub_06, 6, no_error_code);
define_stub!(stub_07, 7, no_error_code);
define_stub!(stub_08, 8, has_error_code);
define_stub!(stub_09, 9, no_error_code);
define_stub!(stub_10, 10, has_error_code);
define_stub!(stub_11, 11, has_error_code);
define_stub!(stub_12, 12, has_error_code);
define_stub!(stub_13, 13, has_error_code);
define_stub!(stub_14, 14, has_error_code);
define_stub!(stub_15, 15, no_error_code);
define_stub!(stub_16, 16, no_error_code);
define_stub!(stub_17, 17, has_error_code);
define_stub!(stub_18, 18, no_error_code);
define_stub!(stub_19, 19, no_error_code);
define_stub!(stub_20, 20, no_error_code);
define_stub!(stub_21, 21, has_error_code);
define_stub!(stub_22, 22, no_error_code);
define_stub!(stub_23, 23, no_error_code);
define_stub!(stub_24, 24, no_error_code);
define_stub!(stub_25, 25, no_error_code);
define_stub!(stub_26, 26, no_error_code);
define_stub!(stub_27, 27, no_error_code);
define_stub!(stub_28, 28, no_error_code);
define_stub!(stub_29, 29, has_error_code);
define_stub!(stub_30, 30, has_error_code);
define_stub!(stub_31, 31, no_error_code);
define_stub!(stub_32, 32, no_error_code);
// Software `int 0x80` pushes no CPU error code, same as any other
// software-invoked interrupt gate.
define_stub!(stub_128, 128, no_error_code);

/// Loads the IDT. Safety: caller must call this exactly once, after
/// `gdt::init`, since gates reference `KERNEL_CODE_SELECTOR` and the
/// double-fault gate references the TSS's IST1 set up there.
pub unsafe fn init() {
    let mut idt = IDT.lock();
    idt[0].set(stub_00 as *const () as u64, 0);
    idt[1].set(stub_01 as *const () as u64, 0);
    idt[2].set(stub_02 as *const () as u64, 0);
    idt[3].set(stub_03 as *const () as u64, 0);
    idt[4].set(stub_04 as *const () as u64, 0);
    idt[5].set(stub_05 as *const () as u64, 0);
    idt[6].set(stub_06 as *const () as u64, 0);
    idt[7].set(stub_07 as *const () as u64, 0);
    idt[8].set(stub_08 as *const () as u64, DOUBLE_FAULT_IST_INDEX);
    idt[9].set(stub_09 as *const () as u64, 0);
    idt[10].set(stub_10 as *const () as u64, 0);
    idt[11].set(stub_11 as *const () as u64, 0);
    idt[12].set(stub_12 as *const () as u64, 0);
    idt[13].set(stub_13 as *const () as u64, 0);
    idt[14].set(stub_14 as *const () as u64, 0);
    idt[15].set(stub_15 as *const () as u64, 0);
    idt[16].set(stub_16 as *const () as u64, 0);
    idt[17].set(stub_17 as *const () as u64, 0);
    idt[18].set(stub_18 as *const () as u64, 0);
    idt[19].set(stub_19 as *const () as u64, 0);
    idt[20].set(stub_20 as *const () as u64, 0);
    idt[21].set(stub_21 as *const () as u64, 0);
    idt[22].set(stub_22 as *const () as u64, 0);
    idt[23].set(stub_23 as *const () as u64, 0);
    idt[24].set(stub_24 as *const () as u64, 0);
    idt[25].set(stub_25 as *const () as u64, 0);
    idt[26].set(stub_26 as *const () as u64, 0);
    idt[27].set(stub_27 as *const () as u64, 0);
    idt[28].set(stub_28 as *const () as u64, 0);
    idt[29].set(stub_29 as *const () as u64, 0);
    idt[30].set(stub_30 as *const () as u64, 0);
    idt[31].set(stub_31 as *const () as u64, 0);
    idt[32].set(stub_32 as *const () as u64, 0);
    idt[128].set_user(stub_128 as *const () as u64, 0);

    let pointer = DescriptorTablePointer {
        limit: (core::mem::size_of::<[Entry; 256]>() - 1) as u16,
        base: idt.as_ptr() as u64,
    };
    core::arch::asm!("lidt [{}]", in(reg) &pointer, options(readonly));
}
