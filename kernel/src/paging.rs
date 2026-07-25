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
// No user-mode mappings yet, no per-component address spaces. That's the
// capability-model AddressSpace object in docs/cores/kernel/README.md,
// still spec only. This is just the one address space the kernel itself
// runs in.

use limine::memmap;

use crate::mem::{FRAME_ALLOCATOR, FRAME_SIZE, SpinLock};

pub const PAGE_WRITABLE: u64 = 1 << 1;
pub const PAGE_NO_EXECUTE: u64 = 1 << 63;
const PAGE_PRESENT: u64 = 1 << 0;
const PAGE_HUGE: u64 = 1 << 7; // PS bit; only meaningful at the PD level here
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;

/// PML4 physical address of whatever table set `switch_to` last loaded into
/// CR3. `map_page`/`unmap_page` operate on this, so anything called after
/// `init()` doesn't need to carry a `&PageTables` around everywhere; there
/// is only one address space right now anyway (see module docs above).
static ACTIVE_PML4: SpinLock<Option<u64>> = SpinLock::new(None);

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

struct PageTables {
    pml4_phys: u64,
}

/// Same value Limine reported via HhdmRequest. Set once by `build()`,
/// read by every `map_*` call afterward; never changes after that.
static mut HHDM_OFFSET: u64 = 0;

fn phys_to_virt(phys: u64) -> u64 {
    phys + unsafe { HHDM_OFFSET }
}

/// Allocates a frame from the physical allocator and zeroes it via the
/// *existing* HHDM mapping (Limine's, still active until `switch_to` runs).
/// Safety: caller must ensure HHDM_OFFSET is set and still maps this frame,
/// true for the whole window between `build()` starting and `switch_to`.
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
/// yet. Intermediate levels are always PRESENT|WRITABLE; permissions are
/// enforced at the leaf, not here, same convention the hardware itself is
/// built around.
unsafe fn next_level(table: &mut PageTable, idx: usize) -> u64 {
    let entry = table.0[idx];
    if entry & PAGE_PRESENT != 0 {
        entry & ADDR_MASK
    } else {
        let new_phys = alloc_zeroed_table();
        table.0[idx] = new_phys | PAGE_PRESENT | PAGE_WRITABLE;
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
/// call `switch_to` once this returns. Safety: caller must ensure
/// `hhdm_offset` and `memmap_entries` are the live values from Limine's
/// responses, and that the frame allocator is already initialized.
unsafe fn build(hhdm_offset: u64, memmap_entries: &[&memmap::Entry]) -> PageTables {
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

    PageTables { pml4_phys }
}

/// Loads CR3 with the new hierarchy. Flushes the entire TLB (no PCID in
/// use), which is fine, this only happens once at boot on a single core.
/// Safety: `tables` must cover every virtual address currently executing
/// or about to be dereferenced, i.e. the whole kernel image plus whatever
/// of the current stack the CPU needs next. True for `build`'s output as
/// long as the boot stack Limine handed us lives inside a usable region.
unsafe fn switch_to(tables: &PageTables) {
    core::arch::asm!(
        "mov cr3, {}",
        in(reg) tables.pml4_phys,
        options(nostack, preserves_flags),
    );
    *ACTIVE_PML4.lock() = Some(tables.pml4_phys);
}

/// Builds the kernel's own page tables and switches to them. Called once
/// from `kernel_main`, after the frame allocator and before anything that
/// calls `map_page`/`unmap_page`.
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
    let tables = build(hhdm_offset, memmap_entries);
    switch_to(&tables);
    // `tables` is deliberately leaked here, not dropped: the page table
    // frames it describes now back the running kernel's own address
    // space and must outlive this function. PageTables has no Drop impl,
    // so this is just letting it go out of scope; nothing is freed.
}

/// Maps one 4K page into the currently active address space. Panics if
/// `init()` hasn't run yet, mapping without an active table set is always
/// a bug, not a runtime condition to handle gracefully.
///
/// Safety: caller must ensure `phys` is a frame it owns (typically fresh
/// from `FRAME_ALLOCATOR`) and that `virt` isn't already mapped to
/// something else still in use.
pub unsafe fn map_page(virt: u64, phys: u64, flags: u64) {
    let pml4_phys = ACTIVE_PML4.lock().expect("rose: map_page before paging::init");
    map_4k(pml4_phys, virt, phys, flags);
    invlpg(virt);
}

/// Unmaps a single 4K page from the currently active address space and
/// invalidates its TLB entry. Does not free the underlying physical frame,
/// that's the caller's responsibility (via `FRAME_ALLOCATOR::free`) since
/// this layer has no way to know whether the frame is still referenced
/// from anywhere else.
///
/// Safety: caller must ensure `virt` was previously mapped and nothing
/// still holds a live reference through it.
pub unsafe fn unmap_page(virt: u64) {
    let pml4_phys = ACTIVE_PML4.lock().expect("rose: unmap_page before paging::init");
    let pml4 = table_at(pml4_phys);
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

unsafe fn invlpg(virt: u64) {
    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
}
