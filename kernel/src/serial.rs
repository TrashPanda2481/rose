//! COM1 UART. Port I/O, no memory mapping needed. Works identically under
//! QEMU and VirtualBox, which is why it's the first hardware interface.

const COM1: u16 = 0x3F8;

#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    val
}

pub struct Serial;

impl Serial {
    pub fn init() -> Self {
        unsafe {
            outb(COM1 + 1, 0x00); // disable interrupts
            outb(COM1 + 3, 0x80); // enable DLAB (set baud rate divisor)
            outb(COM1 + 0, 0x03); // divisor low byte (38400 baud)
            outb(COM1 + 1, 0x00); // divisor high byte
            outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop bit
            outb(COM1 + 2, 0xC7); // enable FIFO, clear, 14-byte threshold
            outb(COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set
        }
        Serial
    }

    fn is_transmit_empty(&self) -> bool {
        unsafe { inb(COM1 + 5) & 0x20 != 0 }
    }

    pub fn write_byte(&mut self, byte: u8) {
        while !self.is_transmit_empty() {}
        unsafe { outb(COM1, byte) }
    }

    pub fn write_str(&mut self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

impl core::fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        Serial::write_str(self, s);
        Ok(())
    }
}
