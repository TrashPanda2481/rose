// Page tables, v0.1.
//
// Limine hands off with its own page tables already active (that's how we
// got a working higher-half kernel mapping and HHDM at all). This module
// builds a replacement set that we own outright, then switches CR3 to it.
//
// Two things get mapped:
//   1. The kernel image itself, at its actual linked virtual address, one
//      set of permissions per section (text: R+X, rodata: R only,
//      data+bss: R+W+NX). W^X, enforced from the very first kernel-owned
//      page table instead of bolted on later.
//   2. The HHDM range, for MEMMAP_USABLE and MEMMAP_BOOTLOADER_RECLAIMABLE
//      regions. Usable is what the frame allocator hands out. Bootloader-
//      reclaimable has to be included too even though mem.rs doesn't
//      touch it yet: Limine's default boot stack (no StackSizeRequest
//      sent, so we get its 64KiB default) is allocated out of
//      bootloader-reclaimable memory and is still the stack in use the
//      instant CR3 changes. First attempt at this left it out and the
//      very next stack access after the CR3 write (a spilled local in
//      switch_to, before this function even returns) double-faulted;
//      see BUGS.md. Framebuffer, ACPI, and reserved regions stay out for
//      now, nothing dereferences them through HHDM yet. Same offset value
//      Limine gave us is reused, so `phys + hhdm_offset` keeps meaning
//      the same thing across the CR3 switch; nothing in mem.rs needs to
//      change.
//
// AddressSpace is the primitive multiple page-table roots are built on:
// the kernel's own, plus one per usermode component (see usermode.rs) or
// self-test. Every AddressSpace shares PML4 entries 256..512 (HHDM plus
// the kernel image, see NEW_ADDRESS_SPACE_SHARED_RANGE below) so the
// kernel can always run and allocate regardless of which one is
// currently loaded in CR3; entries 0..256 are private per AddressSpace.
// This is conventional (pre-KPTI-style) kernel design: kernel memory is
// present-but-supervisor-only in every AddressSpace, not structurally
// absent from it. That's good enough for ring-3 isolation but doesn't
// yet satisfy the stricter goal in docs/cores/kernel/README.md ("kernel
// memory is structurally unmappable by components, not just off-limits
// by convention"); flagged there as a known v0.1 limitation.
//
// The full capability-model AddressSpace object (Untyped-backed,
// Map/Unmap-able PageTable/Frame caps) in docs/cores/kernel/README.md is
// still spec only. This module is the mechanism it'll eventually sit on
// top of, not that object itself.

use limine::memmap;

use crate::mem::{FRAME_ALLOCATOR, FRAME_SIZE, SpinLock};

pub const PAGE_WRITABLE: u64 = 1 << 1;
pub const PAGE_USER: u64 = 1 << 2;
pub const PAGE_NO_EXECUTE: u64 = 1 << 63;
const PAGE_PRESENT: u64 = 1 << 0;
const PAGE_HUGE: u64 = 1 << 7; // PS bit; only meaningful at the PD level here
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;

/// PML4 index range shared into every AddressSpace by `new_address_space`:
/// index 256 is the HHDM base (`0xffff800000000000` in this sandbox),
/// index 511 is the kernel image's higher-half base
/// (`0xffffffff80000000`). Entries below 256 are the private low/user
/// half and stay zeroed for a fresh AddressSpace.
const SHARED_PML4_RANGE: core::ops::Range<usize> = 256..512;

/// PML4 physical address of the kernel's own page tables, set once by
/// `init()`. `kernel_address_space()` reads this; `new_address_space()`
/// copies the shared range out of it. Never changes after `init()`.
static KERNEL_PML4: SpinLock<Option<u64>> = SpinLock::new(None);

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
}

#[repr(align(4096))]
struct PageTable([u64; 512]);

/// One page-table root: the kernel's own, or one built by
/// `new_address_space` for a usermode component / self-test. Just a
/// PML4 physical address; Copy on purpose, since it's a handle to
/// state that lives in physical memory, not state itself.
#[derive(Clone, Copy)]
pub struct AddressSpace {
    pml4_phys: u64,
}

/// Same value Limine reported via HhdmRequest. Set once by `build()`,
/// read by every `map_*` call afterward; never changes after that.
static mut HHDM_OFFSET: u64 = 0;

pub fn phys_to_virt(phys: u64) -> u64 {
    phys + unsafe { HHDM_OFFSET }
}

/// Allocates a frame from the physical allocator and zeroes it via the
/// permanent HHDM alias. Safe to call both before and after the CR3
/// switch in `init()`: before, it's still Limine's own HHDM; after,
/// it's the kernel's, and either way `phys + HHDM_OFFSET` means the
/// same thing (see module docs). This is also the mechanism a caller
/// can use to write into a not-yet-active AddressSpace's frame, since
/// dereferencing that frame's *target* virtual address doesn't work
/// until that AddressSpace is loaded in CR3, but the HHDM alias always
/// works regardless of which AddressSpace is active.
unsafe fn alloc_zeroed_table() -> u64 {
    let phys = FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: out of memory building page tables");
    let virt = phys_to_virt(phys) as *mut u8;
    core::ptr::write_bytes(virt, 0, FRAME_SIZE as usize);
    phys
}

unsafe fn table_at(phys: u64) -> &'static mut PageTable {
    &mut *(phys_to_virt(phys) as *mut PageTable)
}

fn index(virt: u64, level: u8) -> usize {
    ((virt >> (12 + 9 * level as u64)) & 0x1FF) as usize
}

/// Returns the physical address of the next-level table pointed to by the
/// entry at `idx`, allocating and zeroing a fresh one if it isn't present
/// yet. Intermediate levels are always PRESENT|WRITABLE|USER; permissions
/// are enforced at the leaf, not here, same convention the hardware
/// itself is built around. USER has to be set here too even though every
/// caller so far has been kernel-only: x86 ANDs the U/S bit across every
/// level of the walk, so a leaf PTE's own USER bit is meaningless if any
/// PML4E/PDPTE/PDE above it lacks it. Harmless for existing kernel/HHDM
/// branches, since this only fires the first time a given intermediate
/// entry is allocated; access control still comes entirely from each
/// leaf's own USER bit, this just stops it from being silently
/// overridden by an intermediate table built before user mappings
/// existed.
unsafe fn next_level(table: &mut PageTable, idx: usize) -> u64 {
    let entry = table.0[idx];
    if entry & PAGE_PRESENT != 0 {
        entry & ADDR_MASK
    } else {
        let new_phys = alloc_zeroed_table();
        table.0[idx] = new_phys | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER;
        new_phys
    }
}

unsafe fn map_4k(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
    let pml4 = table_at(pml4_phys);
    let pdpt_phys = next_level(pml4, index(virt, 3));
    let pdpt = table_at(pdpt_phys);
    let pd_phys = next_level(pdpt, index(virt, 2));
    let pd = table_at(pd_phys);
    let pt_phys = next_level(pd, index(virt, 1));
    let pt = table_at(pt_phys);
    pt.0[index(virt, 0)] = (phys & ADDR_MASK) | flags | PAGE_PRESENT;
}

unsafe fn map_2m(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
    let pml4 = table_at(pml4_phys);
    let pdpt_phys = next_level(pml4, index(virt, 3));
    let pdpt = table_at(pdpt_phys);
    let pd_phys = next_level(pdpt, index(virt, 2));
    let pd = table_at(pd_phys);
    pd.0[index(virt, 1)] = (phys & ADDR_MASK) | flags | PAGE_PRESENT | PAGE_HUGE;
}

/// NX bits are silently ignored (or worse, treated as reserved-bit
/// violations) unless EFER.NXE is set. Limine's long-mode setup may or may
/// not have already turned this on; set it ourselves rather than assume.
unsafe fn enable_nx() {
    let mut low: u32;
    let mut high: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") 0xC000_0080u32,
        out("eax") low,
        out("edx") high,
        options(nostack, preserves_flags),
    );
    low |= 1 << 11;
    core::arch::asm!(
        "wrmsr",
        in("ecx") 0xC000_0080u32,
        in("eax") low,
        in("edx") high,
        options(nostack, preserves_flags),
    );
}

fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

/// Maps one section of the kernel image with 4K pages at the given
/// permissions. `virt_start`/`virt_end` come from linker.ld symbols;
/// `kernel_phys_start`/`kernel_virt_start` anchor the virtual-to-physical
/// translation for the whole image (Limine's ExecutableAddressRequest).
unsafe fn map_kernel_section(
    pml4_phys: u64,
    virt_start: u64,
    virt_end: u64,
    kernel_virt_start: u64,
    kernel_phys_start: u64,
    flags: u64,
) {
    let start = align_down(virt_start, FRAME_SIZE);
    let end = align_up(virt_end, FRAME_SIZE);
    let mut virt = start;
    while virt < end {
        let phys = kernel_phys_start + (virt - kernel_virt_start);
        map_4k(pml4_phys, virt, phys, flags);
        virt += FRAME_SIZE;
    }
}

/// Maps `[base, base+length)` at `hhdm_offset + base` using 2MiB pages for
/// the aligned bulk of the region and 4K pages for the unaligned leading
/// and trailing edges.
unsafe fn map_hhdm_range(pml4_phys: u64, base: u64, length: u64, hhdm_offset: u64) {
    let flags = PAGE_WRITABLE | PAGE_NO_EXECUTE;
    let end = base + length;
    let mut phys = base;

    while phys < end && phys % HUGE_PAGE_SIZE != 0 && end - phys >= FRAME_SIZE {
        map_4k(pml4_phys, phys + hhdm_offset, phys, flags);
        phys += FRAME_SIZE;
    }
    while phys + HUGE_PAGE_SIZE <= end {
        map_2m(pml4_phys, phys + hhdm_offset, phys, flags);
        phys += HUGE_PAGE_SIZE;
    }
    while end - phys >= FRAME_SIZE {
        map_4k(pml4_phys, phys + hhdm_offset, phys, flags);
        phys += FRAME_SIZE;
    }
    // Anything smaller than a page left over is unaddressable padding at
    // the tail of the region; the memory map itself never hands out less
    // than a full frame to the allocator either (see mem.rs align_down).
}

/// Builds a fresh set of kernel page tables. Does not switch to them,
/// call `.switch()` once this returns. Safety: caller must ensure
/// `hhdm_offset` and `memmap_entries` are the live values from Limine's
/// responses, and that the frame allocator is already initialized.
unsafe fn build(hhdm_offset: u64, memmap_entries: &[&memmap::Entry]) -> AddressSpace {
    HHDM_OFFSET = hhdm_offset;
    enable_nx();

    let pml4_phys = alloc_zeroed_table();

    let kernel_virt_start = core::ptr::addr_of!(__kernel_start) as u64;
    let text_start = core::ptr::addr_of!(__text_start) as u64;
    let text_end = core::ptr::addr_of!(__text_end) as u64;
    let rodata_start = core::ptr::addr_of!(__rodata_start) as u64;
    let rodata_end = core::ptr::addr_of!(__rodata_end) as u64;
    let data_start = core::ptr::addr_of!(__data_start) as u64;
    let kernel_end = core::ptr::addr_of!(__kernel_end) as u64;

    let kernel_phys_start = crate::EXECUTABLE_ADDRESS_REQUEST
        .response()
        .expect("rose: bootloader gave no kernel physical base")
        .physical_base;

    // Text: read + execute, no write.
    map_kernel_section(
        pml4_phys,
        text_start,
        text_end,
        kernel_virt_start,
        kernel_phys_start,
        0,
    );
    // Rodata: read only, no write, no execute.
    map_kernel_section(
        pml4_phys,
        rodata_start,
        rodata_end,
        kernel_virt_start,
        kernel_phys_start,
        PAGE_NO_EXECUTE,
    );
    // Data + bss: read + write, no execute.
    map_kernel_section(
        pml4_phys,
        data_start,
        kernel_end,
        kernel_virt_start,
        kernel_phys_start,
        PAGE_WRITABLE | PAGE_NO_EXECUTE,
    );

    for entry in memmap_entries {
        if entry.type_ != memmap::MEMMAP_USABLE
            && entry.type_ != memmap::MEMMAP_BOOTLOADER_RECLAIMABLE
        {
            continue;
        }
        map_hhdm_range(pml4_phys, entry.base, entry.length, hhdm_offset);
    }

    AddressSpace { pml4_phys }
}

/// Builds the kernel's own page tables and switches to them. Called once
/// from `kernel_main`, after the frame allocator and before anything that
/// calls `map_page`/`unmap_page`/`new_address_space`.
///
/// Design note (see docs/cores/kernel/README.md boot order): GDT/IDT got
/// built before this, on Limine's page tables, since neither needs its own
/// mappings, they only touch static kernel data that's already mapped.
/// Page tables come next because everything after this point (heap,
/// per-component address spaces, user mode) needs the kernel to own its
/// own hierarchy rather than borrowing the bootloader's indefinitely.
///
/// Safety: same contract as `build`.
pub unsafe fn init(hhdm_offset: u64, memmap_entries: &[&memmap::Entry]) {
    let kernel_as = build(hhdm_offset, memmap_entries);
    kernel_as.switch();
    *KERNEL_PML4.lock() = Some(kernel_as.pml4_phys);
    // kernel_as's page-table frames now back the running kernel's own
    // address space and must outlive this function; they do, since
    // they're addressed by physical frame, not by this local going out
    // of scope. AddressSpace is Copy and has no Drop, so there's
    // nothing to leak here, just a handle ceasing to exist.
}

/// Returns a handle to the kernel's own address space. Panics if called
/// before `init()`, same as every other "used before init" case in this
/// module: not a runtime condition to handle, a bug to fix at the
/// call site.
pub fn kernel_address_space() -> AddressSpace {
    AddressSpace {
        pml4_phys: KERNEL_PML4
            .lock()
            .expect("rose: kernel_address_space before paging::init"),
    }
}

/// Allocates a fresh PML4 and copies the kernel's shared upper half
/// (HHDM + kernel image, PML4 entries 256..512, see SHARED_PML4_RANGE)
/// into it. The lower half (256 entries, this AddressSpace's private
/// low/user range) stays zeroed. Every component gets the kernel
/// mapped in as present-but-supervisor-only, so the kernel can always
/// run and allocate no matter which AddressSpace CR3 currently points
/// at; see module docs for why that's not yet the stricter
/// structurally-unmappable goal.
///
/// Safety: caller must ensure `init()` has already run.
pub unsafe fn new_address_space() -> AddressSpace {
    let pml4_phys = alloc_zeroed_table();
    let kernel_pml4_phys = KERNEL_PML4
        .lock()
        .expect("rose: new_address_space before paging::init");

    // Two live &mut PageTable at once looks like aliasing but isn't:
    // the two physical frames are genuinely distinct (one just
    // allocated above, one already backing the kernel), so this is
    // two disjoint mutable borrows of two disjoint memory regions, not
    // one region borrowed twice.
    let kernel_pml4 = table_at(kernel_pml4_phys);
    let new_pml4 = table_at(pml4_phys);
    for i in SHARED_PML4_RANGE {
        new_pml4.0[i] = kernel_pml4.0[i];
    }

    AddressSpace { pml4_phys }
}

impl AddressSpace {
    /// Loads CR3 with this address space's PML4. Flushes the entire
    /// TLB (no PCID in use). Safety: `self` must cover every virtual
    /// address currently executing or about to be dereferenced right
    /// after the switch, i.e. the whole kernel image plus whatever of
    /// the current stack the CPU needs next. Always true right after
    /// `new_address_space()` (kernel half is shared in) as long as the
    /// caller doesn't jump to a user address before also mapping it.
    pub unsafe fn switch(&self) {
        core::arch::asm!(
            "mov cr3, {}",
            in(reg) self.pml4_phys,
            options(nostack, preserves_flags),
        );
    }

    /// Maps one 4K page into this address space.
    ///
    /// Safety: caller must ensure `phys` is a frame it owns (typically
    /// fresh from `FRAME_ALLOCATOR`) and that `virt` isn't already
    /// mapped to something else still in use. If this AddressSpace
    /// isn't the one currently loaded in CR3, the mapping still takes
    /// effect (it's written straight into the page tables via their
    /// HHDM alias) but won't be visible to code running under it until
    /// `.switch()` loads it; see usermode.rs for the pattern of mapping
    /// a not-yet-active AddressSpace, then writing through
    /// `phys_to_virt` rather than through the target virtual address,
    /// then switching.
    pub unsafe fn map_page(&self, virt: u64, phys: u64, flags: u64) {
        map_4k(self.pml4_phys, virt, phys, flags);
        // Targets whatever's currently loaded in CR3, which may not be
        // `self` if this AddressSpace isn't active yet. Harmless
        // either way: if `self` isn't active, there's no stale TLB
        // entry for this virt to invalidate in the first place, and
        // `.switch()` does an unconditional full flush regardless of
        // which AddressSpace it's switching to.
        invlpg(virt);
    }

    /// Unmaps a single 4K page from this address space and invalidates
    /// its TLB entry (same currently-loaded-CR3 caveat as `map_page`).
    /// Does not free the underlying physical frame, that's the
    /// caller's responsibility (via `FRAME_ALLOCATOR::free`) since this
    /// layer has no way to know whether the frame is still referenced
    /// from anywhere else.
    ///
    /// Safety: caller must ensure `virt` was previously mapped in this
    /// address space and nothing still holds a live reference through
    /// it.
    pub unsafe fn unmap_page(&self, virt: u64) {
        let pml4 = table_at(self.pml4_phys);
        if pml4.0[index(virt, 3)] & PAGE_PRESENT == 0 {
            return;
        }
        let pdpt = table_at(pml4.0[index(virt, 3)] & ADDR_MASK);
        if pdpt.0[index(virt, 2)] & PAGE_PRESENT == 0 {
            return;
        }
        let pd = table_at(pdpt.0[index(virt, 2)] & ADDR_MASK);
        if pd.0[index(virt, 1)] & PAGE_PRESENT == 0 || pd.0[index(virt, 1)] & PAGE_HUGE != 0 {
            return;
        }
        let pt = table_at(pd.0[index(virt, 1)] & ADDR_MASK);
        pt.0[index(virt, 0)] = 0;
        invlpg(virt);
    }

    /// Read-only table walk checking whether `virt` is PRESENT in this
    /// address space, without ever dereferencing `virt` itself, only
    /// the page-table entries that describe it (via their HHDM alias,
    /// same as every other table walk in this module). Safe to call on
    /// an address that would fault if actually accessed; that's the
    /// point, this is how a caller proves a page is or isn't mapped
    /// before touching it. Treats a present huge (2MiB) leaf at the PD
    /// level as mapped without walking further, unlike `unmap_page`
    /// which just leaves huge leaves alone.
    ///
    /// Safety: caller must ensure this AddressSpace was actually built
    /// by `new_address_space` or is the kernel's own; walking garbage
    /// physical addresses through `table_at` is undefined otherwise.
    pub unsafe fn is_mapped(&self, virt: u64) -> bool {
        let pml4 = table_at(self.pml4_phys);
        if pml4.0[index(virt, 3)] & PAGE_PRESENT == 0 {
            return false;
        }
        let pdpt = table_at(pml4.0[index(virt, 3)] & ADDR_MASK);
        if pdpt.0[index(virt, 2)] & PAGE_PRESENT == 0 {
            return false;
        }
        let pd = table_at(pdpt.0[index(virt, 2)] & ADDR_MASK);
        if pd.0[index(virt, 1)] & PAGE_PRESENT == 0 {
            return false;
        }
        if pd.0[index(virt, 1)] & PAGE_HUGE != 0 {
            return true;
        }
        let pt = table_at(pd.0[index(virt, 1)] & ADDR_MASK);
        pt.0[index(virt, 0)] & PAGE_PRESENT != 0
    }
}

unsafe fn invlpg(virt: u64) {
    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
}
