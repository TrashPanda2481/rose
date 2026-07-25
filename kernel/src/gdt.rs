// Global Descriptor Table, v0.1.
//
// Limine hands off with its own GDT already loaded (that's how we got into
// 64-bit mode at all). We replace it with our own so we control the
// selectors that everything else (IDT gates, far returns, segment loads)
// references from here on.
//
// Includes a minimal TSS purely for its IST1 stack, used by the double
// fault handler in idt.rs. A fault while already handling a fault (stack
// overflow, corrupted RSP) needs a fresh known-good stack to report
// anything instead of silently triple-faulting. This is worth having
// before page-table work starts, the most pointer-heavy code so far.
// No ring 3 descriptors yet, there's no component model to run in ring 3.

use core::arch::asm;
use core::mem::size_of;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x18;

/// IDT `ist` field value that routes double faults through TSS.ist[0]
/// (IST1). 0 means "use whatever stack was already active."
pub const DOUBLE_FAULT_IST_INDEX: u8 = 1;

const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 4; // 16 KiB
static mut DOUBLE_FAULT_STACK: [u8; DOUBLE_FAULT_STACK_SIZE] = [0; DOUBLE_FAULT_STACK_SIZE];

#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

impl Tss {
    const fn empty() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: 0,
        }
    }
}

static mut TSS: Tss = Tss::empty();

const fn code_data_descriptor(access: u8, flags: u8) -> u64 {
    // Base and limit are meaningless for code/data segments in long mode
    // (the CPU ignores them for anything but the access/flags bits), so
    // both are left at 0. Only access and flags carry real information.
    let mut d: u64 = 0;
    d |= (access as u64) << 40;
    d |= (flags as u64) << 52;
    d
}

// Access byte: P=1, DPL=00, S=1 (code/data, not system), Type=1010 (execute/read).
const CODE_ACCESS: u8 = 0x9A;
// Access byte: P=1, DPL=00, S=1, Type=0010 (read/write).
const DATA_ACCESS: u8 = 0x92;
// Flags nibble (AVL, L, D/B, G): L=1 marks a 64-bit code segment. D/B must
// be 0 when L=1, that combination is reserved and would be misinterpreted.
const CODE_FLAGS: u8 = 0b1010;
// Flags nibble for the data segment: L doesn't apply to data segments, so
// just G=1 by convention. Nothing reads it in long mode.
const DATA_FLAGS: u8 = 0b1000;
// TSS access byte: P=1, DPL=00, Type=1001 (64-bit TSS, available).
const TSS_ACCESS: u8 = 0x89;

/// TSS descriptor is a 16-byte "system segment" (needs the full 64-bit
/// base), unlike the plain 8-byte code/data descriptors above. Built from
/// two u64 words so it can sit in the same flat GDT array.
fn tss_descriptor(base: u64, limit: u32) -> [u64; 2] {
    let low = ((limit as u64) & 0xFFFF)
        | ((base & 0xFFFF) << 16)
        | (((base >> 16) & 0xFF) << 32)
        | ((TSS_ACCESS as u64) << 40)
        | ((((limit as u64) >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    let high = (base >> 32) & 0xFFFF_FFFF;
    [low, high]
}

static mut GDT: [u64; 5] = [
    0,                                          // 0x00: null descriptor, required
    code_data_descriptor(CODE_ACCESS, CODE_FLAGS), // 0x08: kernel code
    code_data_descriptor(DATA_ACCESS, DATA_FLAGS), // 0x10: kernel data
    0,                                          // 0x18: TSS descriptor low, filled at init
    0,                                          // TSS descriptor high, filled at init
];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Loads our GDT, reloads every segment register including CS, and loads
/// the task register so IST1 becomes available to the IDT. Safety: caller
/// must call this exactly once, early in boot, before anything relies on
/// segment state or the double-fault IST.
pub unsafe fn init() {
    let stack_top =
        core::ptr::addr_of!(DOUBLE_FAULT_STACK) as u64 + DOUBLE_FAULT_STACK_SIZE as u64;
    TSS.ist[(DOUBLE_FAULT_IST_INDEX - 1) as usize] = stack_top;
    TSS.iomap_base = size_of::<Tss>() as u16; // no I/O bitmap, point past the TSS

    let tss_addr = core::ptr::addr_of!(TSS) as u64;
    let tss_desc = tss_descriptor(tss_addr, (size_of::<Tss>() - 1) as u32);
    GDT[3] = tss_desc[0];
    GDT[4] = tss_desc[1];

    let pointer = DescriptorTablePointer {
        limit: (size_of::<[u64; 5]>() - 1) as u16,
        base: core::ptr::addr_of!(GDT) as u64,
    };
    asm!("lgdt [{}]", in(reg) &pointer, options(readonly));

    // CS can't be reloaded with a plain `mov`, the CPU only picks up a new
    // code segment on a control transfer. The standard trick: push the
    // target selector and a return address, then far-return into it.
    asm!(
        "push {sel}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",
        sel = in(reg) KERNEL_CODE_SELECTOR as u64,
        tmp = lateout(reg) _,
        options(preserves_flags),
    );

    // Data/stack segments do accept a plain `mov`.
    asm!(
        "mov ss, {sel:x}",
        "mov ds, {sel:x}",
        "mov es, {sel:x}",
        "mov fs, {sel:x}",
        "mov gs, {sel:x}",
        sel = in(reg) KERNEL_DATA_SELECTOR,
        options(nostack, preserves_flags),
    );

    asm!("ltr {sel:x}", sel = in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
}
