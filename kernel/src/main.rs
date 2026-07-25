#![no_std]
#![no_main]

mod gdt;
mod idt;
mod mem;
mod serial;

use core::fmt::Write;
use core::panic::PanicInfo;
use limine::BaseRevision;
use limine::RequestsEndMarker;
use limine::RequestsStartMarker;
use limine::memmap;
use limine::request::HhdmRequest;
use limine::request::MemmapRequest;

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

/// This is the only place authority comes from nothing (see kernel core
/// invariants doc, boot handoff section). Limine hands control here.
#[unsafe(no_mangle)]
unsafe extern "C" fn kernel_main() -> ! {
    assert!(BASE_REVISION.is_supported());

    let mut com1 = serial::Serial::init();
    let _ = writeln!(com1, "rose: boot ok");
    let _ = writeln!(com1, "rose: base revision supported");
    let _ = writeln!(com1, "rose: hello, hardware");

    let memmap_response = MEMMAP_REQUEST
        .response()
        .expect("rose: bootloader gave no memory map");
    let entries = memmap_response.entries();
    let _ = writeln!(com1, "rose: memory map: {} entries", entries.len());

    let mut usable_bytes: u64 = 0;
    for entry in entries {
        let _ = writeln!(
            com1,
            "rose:   base={:#018x} length={:#012x} type={}",
            entry.base,
            entry.length,
            memmap_type_name(entry.type_)
        );
        if entry.type_ == memmap::MEMMAP_USABLE {
            usable_bytes += entry.length;
        }
    }
    let _ = writeln!(com1, "rose: usable memory: {} KiB", usable_bytes / 1024);

    let hhdm_response = HHDM_REQUEST
        .response()
        .expect("rose: bootloader gave no HHDM offset");
    let hhdm_offset = hhdm_response.offset;
    let _ = writeln!(com1, "rose: hhdm offset={:#018x}", hhdm_offset);

    unsafe {
        mem::init(hhdm_offset, entries);
    }

    frame_allocator_selftest(&mut com1);

    unsafe {
        gdt::init();
        idt::init();
    }
    let _ = writeln!(com1, "rose: gdt/idt loaded");

    idt_selftest(&mut com1);

    loop {
        core::arch::asm!("hlt");
    }
}

/// Deliberately triggers a breakpoint exception. If GDT/IDT/TSS are wired
/// correctly, the handler in idt.rs logs it and returns here normally,
/// proving the whole chain works without having to crash the kernel to
/// prove it, same spirit as the frame allocator self-test above.
fn idt_selftest(com1: &mut serial::Serial) {
    let _ = writeln!(com1, "rose: idt self-test: triggering breakpoint");
    unsafe {
        core::arch::asm!("int3");
    }
    let _ = writeln!(com1, "rose: idt self-test: resumed after breakpoint, ok");
}

fn frame_allocator_selftest(com1: &mut serial::Serial) {
    let mut allocator = mem::FRAME_ALLOCATOR.lock();
    let _ = writeln!(
        com1,
        "rose: frame allocator: {} total frames, {} free",
        allocator.total_count(),
        allocator.free_count()
    );

    let a = allocator.alloc();
    let b = allocator.alloc();
    let c = allocator.alloc();
    let _ = writeln!(com1, "rose: alloc test: got {:?} {:?} {:?}", a, b, c);

    if let Some(phys) = b {
        unsafe {
            allocator.free(phys);
        }
    }
    let d = allocator.alloc();
    let _ = writeln!(
        com1,
        "rose: alloc after free: got {:?}, expected same as freed frame",
        d
    );

    let _ = writeln!(
        com1,
        "rose: frame allocator: {} free after self-test",
        allocator.free_count()
    );
}

/// Human-readable label for a Limine memmap entry type. Serial-log only for
/// now; the frame allocator built on top of this will care about usable vs.
/// not, not the exact label.
fn memmap_type_name(type_: u64) -> &'static str {
    match type_ {
        memmap::MEMMAP_USABLE => "usable",
        memmap::MEMMAP_RESERVED => "reserved",
        memmap::MEMMAP_ACPI_RECLAIMABLE => "acpi_reclaimable",
        memmap::MEMMAP_ACPI_NVS => "acpi_nvs",
        memmap::MEMMAP_BAD_MEMORY => "bad_memory",
        memmap::MEMMAP_BOOTLOADER_RECLAIMABLE => "bootloader_reclaimable",
        memmap::MEMMAP_EXECUTABLE_AND_MODULES => "kernel_and_modules",
        memmap::MEMMAP_FRAMEBUFFER => "framebuffer",
        memmap::MEMMAP_MAPPED_RESERVED => "mapped_reserved",
        _ => "unknown",
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Panic path has to work even before any real driver exists, so it
    // talks to the UART directly rather than going through anything else.
    let mut com1 = serial::Serial::init();
    let _ = writeln!(com1, "rose: PANIC: {}", info);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}
