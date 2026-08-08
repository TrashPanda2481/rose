// PCI enumeration, v0.1. Read-only bus discovery through the legacy PCI
// configuration mechanism #1 (I/O ports 0xCF8/0xCFC), the one access
// method every PC-compatible x86 system has supported since the original
// PCI spec; no ACPI or MMCONFIG/ECAM parsing needed first. This is tier 1
// of the hardware detection model: walk the bus, read what a compliant
// device already publishes about itself (vendor/device id, class code,
// subclass, programming interface), and log it. Nothing here sends a
// device a single command, touches a BAR, or assumes a driver exists;
// enumeration only confirms presence and self-declared purpose, the same
// verify-before-commit discipline the Configure syscall already
// established (peek before claim; here, read the vendor id before ever
// reading anything else about a slot).
//
// Deliberately not done yet: PCIe extended configuration space (needs
// ACPI's MCFG table, which needs ACPI parsing, neither of which exist in
// this kernel yet), capability list walking, and anything that writes to
// a device. Read-only sensing only, matching the tier 1 boundary: detect
// and log, don't touch.

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

const VENDOR_ABSENT: u16 = 0xFFFF;

#[inline(always)]
unsafe fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Builds the 32-bit CONFIG_ADDRESS value for one dword-aligned register
/// in one function's configuration space. `offset` is truncated down to
/// the nearest 4-byte boundary, matching the hardware's own alignment
/// requirement.
fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    (1 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Reads one dword from a function's configuration space. Safety: caller
/// must not call this concurrently with another config space access from
/// another CPU; there's only one CONFIG_ADDRESS/CONFIG_DATA pair shared
/// by the whole bus. Not reachable today since this kernel has no
/// cross-core concurrency yet, noted for when it does.
unsafe fn read_config(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    unsafe {
        outl(CONFIG_ADDRESS, config_address(bus, device, function, offset));
        inl(CONFIG_DATA)
    }
}

/// Everything read off one present PCI function. Fields are exactly what
/// the device publishes about itself at config space offsets 0x00 and
/// 0x08; nothing here is inferred or guessed.
pub struct DeviceInfo {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision_id: u8,
}

/// Walks every bus/device/function slot the legacy mechanism can address
/// (256 buses x 32 devices x 8 functions) and calls `on_device` once for
/// each slot that reports a real vendor id. The vendor id read is
/// deliberately the only thing checked before deciding whether a slot is
/// worth reading further: an absent slot returns 0xFFFF for every
/// register, so confirming presence first means every other field read
/// only happens for a function that's actually there. No assumption is
/// made about which buses or devices exist ahead of time, and no
/// multi-function shortcut is taken either; every slot is probed the
/// same way regardless of what was found on any other slot, since
/// nothing about this bus's topology is trusted until read.
pub fn enumerate(mut on_device: impl FnMut(DeviceInfo)) {
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let vendor_device = unsafe { read_config(bus, device, function, 0x00) };
                let vendor_id = (vendor_device & 0xFFFF) as u16;
                if vendor_id == VENDOR_ABSENT {
                    continue;
                }
                let device_id = (vendor_device >> 16) as u16;

                let class_reg = unsafe { read_config(bus, device, function, 0x08) };
                let revision_id = (class_reg & 0xFF) as u8;
                let prog_if = ((class_reg >> 8) & 0xFF) as u8;
                let subclass = ((class_reg >> 16) & 0xFF) as u8;
                let class_code = ((class_reg >> 24) & 0xFF) as u8;

                on_device(DeviceInfo {
                    bus,
                    device,
                    function,
                    vendor_id,
                    device_id,
                    class_code,
                    subclass,
                    prog_if,
                    revision_id,
                });
            }
        }
    }
}
