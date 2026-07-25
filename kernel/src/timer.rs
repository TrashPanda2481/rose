// PIT (8253/8254) timer, v0.1.
//
// Channel 0, mode 3 (square wave), configured for 100Hz. That's the
// kernel's only notion of time right now: a monotonically increasing
// tick counter, incremented from the IRQ0 handler in idt.rs. No
// wall-clock, no RTC read yet; "time since boot" is ticks * 10ms.
//
// 100Hz chosen so a boot self-test can count something meaningful
// (half a second is 50 ticks) without spamming serial or wasting cycles
// once a scheduler actually needs a timeslice interrupt. Nothing depends
// on this exact frequency yet, it can change freely later.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

const PIT_FREQUENCY_HZ: u32 = 1_193_182;
const TARGET_HZ: u32 = 100;

const PIT_CHANNEL0_DATA: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;

static TICKS: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

/// Programs the PIT and enables interrupts (STI). Safety: caller must
/// have already loaded the IDT and remapped/masked the PIC
/// (`pic::init`), since the very first tick after STI expects both to
/// already be ready to receive it.
pub unsafe fn init() {
    let divisor = (PIT_FREQUENCY_HZ / TARGET_HZ) as u16;
    unsafe {
        outb(PIT_COMMAND, 0x36); // channel 0, lobyte/hibyte access, mode 3, binary
        outb(PIT_CHANNEL0_DATA, (divisor & 0xFF) as u8);
        outb(PIT_CHANNEL0_DATA, (divisor >> 8) as u8);
        core::arch::asm!("sti");
    }
}

/// Called only from the IRQ0 handler in idt.rs, once per tick. Logs once
/// on the very first tick, as a boot confirmation that IRQ0 is actually
/// reaching the handler end to end (PIC remap + IDT vector 32 + PIT
/// programming all correct); silent every tick after that on purpose.
pub fn on_tick() {
    let previous = TICKS.fetch_add(1, Ordering::Relaxed);
    if previous == 0 {
        let mut com1 = crate::serial::Serial::init();
        let _ = writeln!(com1, "rose: timer: first irq0 tick received");
    }
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Exact at 100Hz (1000/100 = 10 with no remainder); revisit the math if
/// TARGET_HZ ever changes to something that doesn't divide 1000 evenly.
pub fn ms_since_boot() -> u64 {
    ticks() * (1000 / TARGET_HZ as u64)
}
