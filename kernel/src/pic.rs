// 8259 Programmable Interrupt Controller, v0.1.
//
// Remapped so IRQ0-7 land on vectors 32-39 and IRQ8-15 on 40-47, clear of
// the CPU exception range (0-31); the PIC's power-on default (IRQ0 at
// vector 8) collides with #DF, #TS, etc. and makes a real hardware
// interrupt indistinguishable from a CPU exception at the IDT level.
//
// Both master and slave are fully programmed even though nothing uses the
// slave yet (no IRQ8-15 source unmasked): the 8259 spec requires
// completing the full ICW1-4 sequence on both chips before either is
// usable, since they're cascaded through IRQ2.
//
// Only IRQ0 (timer) is unmasked for now. Everything else stays masked
// until something actually drives it.

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

pub const IRQ0_TIMER_VECTOR: u64 = 32;

#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

/// Real hardware needs a few microseconds between successive PIC command
/// writes for the chip to catch up; the traditional trick is an out to an
/// unused port (0x80, POST diagnostic port on PC/AT) purely for the
/// delay. QEMU/VirtualBox don't need it, real silicon might; costs
/// nothing to keep.
unsafe fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// Remaps both PICs and masks everything except IRQ0. Safety: caller must
/// call this once, before enabling interrupts (STI), with the IDT already
/// loaded so vector 32 has somewhere to go the instant IRQ0 is unmasked.
pub unsafe fn init() {
    unsafe {
        // ICW1: start init sequence, expect ICW4.
        outb(MASTER_COMMAND, 0x11);
        io_wait();
        outb(SLAVE_COMMAND, 0x11);
        io_wait();

        // ICW2: vector offsets.
        outb(MASTER_DATA, 32); // IRQ0-7 -> vectors 32-39
        io_wait();
        outb(SLAVE_DATA, 40); // IRQ8-15 -> vectors 40-47
        io_wait();

        // ICW3: cascade wiring. Master: slave lives on IRQ2 (bit mask
        // 0x04). Slave: its own cascade identity, 2.
        outb(MASTER_DATA, 0x04);
        io_wait();
        outb(SLAVE_DATA, 0x02);
        io_wait();

        // ICW4: 8086 mode.
        outb(MASTER_DATA, 0x01);
        io_wait();
        outb(SLAVE_DATA, 0x01);
        io_wait();

        // Mask everything except IRQ0 (bit 0 clear = unmasked).
        outb(MASTER_DATA, 0xFE);
        outb(SLAVE_DATA, 0xFF);
    }
}

/// Signals end-of-interrupt. Every hardware IRQ handler must call this
/// before returning or the PIC will never deliver another one at or
/// below this priority. Slave IRQs (8-15) need an EOI to both chips
/// since they're cascaded; master-only IRQs (0-7) just need the master.
pub unsafe fn send_eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, 0x20);
        }
        outb(MASTER_COMMAND, 0x20);
    }
}
