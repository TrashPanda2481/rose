#![no_std]
#![no_main]

mod serial;

use core::fmt::Write;
use core::panic::PanicInfo;
use limine::BaseRevision;
use limine::RequestsEndMarker;
use limine::RequestsStartMarker;
use limine::memmap;
use limine::request::MemmapRequest;

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

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

    loop {
        core::arch::asm!("hlt");
    }
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
