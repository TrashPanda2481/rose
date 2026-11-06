// Root task boot handoff, Phase 1 increment 1.
//
// This is the real version of what usermode_selftest stood in for: the
// one place the kernel turns its own authority into a running component
// and then stops creating components itself. kernel_main brings up every
// primitive, runs the in-kernel self-tests, then calls handoff() here and
// idles. handoff builds exactly one privileged root task (a fresh
// AddressSpace, a CSpace, an Untyped cap over a bounded pool of remaining
// memory, and a read-only BootInfo page), starts it in ring 3, and
// returns. Every later component descends from what this task holds.
//
// The embedded root program (hand-written ring-3 asm below) proves the
// handoff worked end to end: it reads BootInfo, retypes one Frame out of
// its own Untyped, Frame-Maps that frame into its own AddressSpace,
// writes and reads back a pattern, then emits one greppable success line
// through SYS_ROOT_REPORT. A failure anywhere short of that faults
// loudly (an unmapped write is a #PF the idt handler logs), so the
// success line only ever prints on the fully-working path.
//
// Scope cuts for this increment live in
// docs/cores/kernel/plans/root-task-handoff.md: no external loader (the
// program is embedded, same as the old self-test), a large bounded
// Untyped pool rather than a contiguous-region one, no device caps, no
// child components yet.

use core::fmt::Write;

use crate::console;
use crate::cspace;
use crate::mem;
use crate::paging;
use crate::scheduler;
use crate::untyped;
use abi::{Capability, KernelObjectId, ObjectType, Rights};
use alloc::boxed::Box;

// Fixed low, page-aligned, private virtual addresses for the root
// program's pages in its fresh AddressSpace. All far below the shared
// higher-half range, so each gets its own PML4 branch and none collide
// with the kernel or the HHDM. Each is also hardcoded as an immediate in
// the ring-3 asm below; if one changes, its asm literal must change with
// it. SCRATCH_VADDR has no Rust const here on purpose: the kernel never
// maps it, the root program does, so it exists only as an asm literal
// (0xa00000).
const CODE_VADDR: u64 = 0x0000_0000_0040_0000; // 4 MiB: root code (R+X+U)
const STACK_VADDR: u64 = 0x0000_0000_0060_0000; // 6 MiB: root stack (R+W+U+NX)
const BOOTINFO_VADDR: u64 = 0x0000_0000_0080_0000; // 8 MiB: BootInfo (R+U, read-only)

// CSpace slots the handoff grants into the root's own CSpace, matching
// the cptr immediates in the ring-3 asm below.
const UNTYPED_SLOT: abi::CPtr = 1;
const ROOT_AS_SLOT: abi::CPtr = 2;

// Size of the Untyped pool granted to the root, in frames. "Large
// bounded pool" per the plan's decision 3; a contiguous-region Untyped
// over all remaining memory is increment 1b. 1024 frames is 4 MiB, far
// more than the root's single retype needs and tiny against the memory
// the allocator reports.
const ROOT_UNTYPED_FRAMES: usize = 1024;

/// Builds the one privileged root task and starts it in ring 3. Returns
/// so kernel_main can idle; the root runs once kernel_main yields the
/// CPU to it. `usable_bytes`/`hhdm_offset` come straight from
/// kernel_main's own bring-up; the framebuffer geometry is read here
/// from the Limine response, same as console::init read it earlier.
pub fn handoff(com1: &mut console::Dual, usable_bytes: u64, hhdm_offset: u64) {
    let bootinfo = build_bootinfo(usable_bytes, hhdm_offset);

    let root_as = unsafe { paging::new_address_space() };

    // Code page: R+X+U, never WRITABLE. Copied in through the frame's
    // permanent HHDM alias, not through CODE_VADDR (not mapped in the
    // currently-active kernel AddressSpace), same approach as the old
    // usermode_selftest.
    let code_phys = alloc_frame();
    unsafe { root_as.map_page(CODE_VADDR, code_phys, paging::PAGE_USER) };
    let program = program_bytes();
    unsafe {
        let dst = paging::phys_to_virt(code_phys) as *mut u8;
        core::ptr::copy_nonoverlapping(program.as_ptr(), dst, program.len());
    }

    // Stack page: R+W+U+NX. The program barely uses it (its syscalls trap
    // onto the kernel stack via TSS.RSP0), but a ring-3 task gets a real
    // stack regardless.
    let stack_phys = alloc_frame();
    unsafe {
        root_as.map_page(
            STACK_VADDR,
            stack_phys,
            paging::PAGE_USER | paging::PAGE_WRITABLE | paging::PAGE_NO_EXECUTE,
        );
    }
    let stack_top = STACK_VADDR + mem::FRAME_SIZE;

    // BootInfo page: R+U, read-only, NX. Written in through the HHDM
    // alias; the root only ever reads it.
    let bootinfo_phys = alloc_frame();
    unsafe {
        root_as.map_page(
            BOOTINFO_VADDR,
            bootinfo_phys,
            paging::PAGE_USER | paging::PAGE_NO_EXECUTE,
        );
        let dst = paging::phys_to_virt(bootinfo_phys) as *mut abi::BootInfo;
        dst.write(bootinfo);
    }

    // The root's own CSpace, granted exactly two capabilities. Rights are
    // minimal and intentional (plan risk note): the Untyped gets
    // READ|WRITE|MAP (WRITE is what dispatch_untyped's Retype check
    // requires), the AddressSpace cap gets MAP only (enough for the
    // root's own Frame-Map into itself, nothing more).
    let mut root_cspace = Box::new(cspace::CSpace::new());

    let untyped = untyped::UntypedObject::new(ROOT_UNTYPED_FRAMES)
        .expect("rose: root task: out of memory building Untyped pool");
    let untyped_id = untyped::register_untyped(untyped);
    root_cspace
        .grant_root(
            UNTYPED_SLOT,
            Capability {
                object_ref: KernelObjectId(untyped_id),
                object_type: ObjectType::Untyped,
                rights: Rights::READ.union(Rights::WRITE).union(Rights::MAP),
                badge: 0,
            },
        )
        .expect("rose: root task: granting Untyped cap failed");

    root_cspace
        .grant_root(
            ROOT_AS_SLOT,
            Capability {
                object_ref: KernelObjectId(root_as.raw()),
                object_type: ObjectType::AddressSpace,
                rights: Rights::MAP,
                badge: 0,
            },
        )
        .expect("rose: root task: granting AddressSpace cap failed");

    let _ = writeln!(
        com1,
        "rose: root task: handoff built, entering ring 3 at {:#018x}",
        CODE_VADDR
    );

    // Start the root as a real scheduled ring-3 task in its own
    // AddressSpace and CSpace. spawn_user is the primitive
    // thread::configure itself drives; the kernel is the authority origin
    // here, so it wires the task directly rather than going through the
    // cap-checked Configure syscall path. The root first runs when
    // kernel_main yields to it.
    scheduler::spawn_user(CODE_VADDR, stack_top, root_as, root_cspace);
}

fn alloc_frame() -> u64 {
    mem::FRAME_ALLOCATOR
        .lock()
        .alloc()
        .expect("rose: root task: out of memory")
}

fn build_bootinfo(usable_bytes: u64, hhdm_offset: u64) -> abi::BootInfo {
    let mut info = abi::BootInfo {
        magic: abi::BootInfo::MAGIC,
        usable_bytes,
        hhdm_offset,
        fb_addr: 0,
        fb_width: 0,
        fb_height: 0,
        fb_pitch: 0,
        fb_bpp: 0,
        _reserved: 0,
    };
    if let Some(fb) = crate::FRAMEBUFFER_REQUEST
        .response()
        .and_then(|r| r.framebuffers().first().copied())
    {
        info.fb_addr = fb.address() as u64;
        info.fb_width = fb.width as u32;
        info.fb_height = fb.height as u32;
        info.fb_pitch = fb.pitch as u32;
        info.fb_bpp = fb.bpp as u16;
    }
    info
}

/// Called from idt.rs for the root program's SYS_ROOT_REPORT syscall.
/// `magic` is what the root read out of BootInfo; `readback`/`expected`
/// are the value it read back from its freshly mapped frame and the
/// value it wrote there. Prints the one greppable success line the smoke
/// test and a VirtualBox boot both look for, or a FAILED variant.
pub fn on_report(magic: u64, readback: u64, expected: u64) {
    let mut com1 = console::Dual::init();
    let bootinfo_ok = magic == abi::BootInfo::MAGIC;
    let rw_ok = readback == expected;
    if bootinfo_ok && rw_ok {
        let _ = writeln!(
            com1,
            "rose: root task: handoff ok (bootinfo read, untyped retype+map+rw confirmed)"
        );
    } else {
        let _ = writeln!(
            com1,
            "rose: root task: handoff FAILED (bootinfo {}, rw {})",
            if bootinfo_ok { "ok" } else { "MISMATCH" },
            if rw_ok { "ok" } else { "MISMATCH" }
        );
    }
}

// Hand-written ring-3 machine code, same convention as usermode.rs's old
// self-test program: a dedicated .rodata section with start/end symbols,
// hand-placed into a USER-mapped page by handoff's memcpy. Straight-line,
// no branches but the final spin. Register values and the cptr/vaddr
// immediates are hardcoded to match the consts above and the grants
// handoff performs; keep them in sync.
//
//   r12 <- [0x800000]                       BootInfo.magic (offset 0)
//   RETYPE    slot1(untyped) -> slot3, Frame(1)         rax=0x1004
//   FRAME_MAP slot3(frame) -> slot2(own AS) @ 0xa00000,
//             flags=WRITABLE|NO_EXECUTE(3)              rax=0x1006
//   write pattern to 0xa00000, r13 <- readback
//   REPORT    rdi=magic rsi=readback rdx=expected       rax=0x2000
//   spin
core::arch::global_asm!(
    ".section .rodata.root_program, \"a\"",
    ".global root_program_start",
    ".global root_program_end",
    "root_program_start:",
    // r12 = BootInfo.magic (offset 0 of the read-only BootInfo page)
    "mov rax, 0x800000",
    "mov r12, [rax]",
    // RETYPE slot1 -> slot3, Frame (ObjectType::Frame = 1)
    "mov rdi, 1",
    "mov rsi, 1",
    "mov rdx, 3",
    "mov rax, 0x1004",
    "int 0x80",
    // FRAME_MAP frame slot3 -> own-AS slot2 @ 0xa00000,
    // flags = MAP_FLAG_WRITABLE | MAP_FLAG_NO_EXECUTE (3)
    "mov rdi, 3",
    "mov rsi, 2",
    "mov rdx, 0xa00000",
    "mov r10, 3",
    "mov rax, 0x1006",
    "int 0x80",
    // write the pattern through the freshly mapped page, read it back
    "mov rax, 0xa00000",
    "mov rbx, 0x5a5a1234f00dd00d",
    "mov [rax], rbx",
    "mov r13, [rax]",
    // REPORT: rdi=magic, rsi=readback, rdx=expected pattern
    "mov rdi, r12",
    "mov rsi, r13",
    "mov rdx, 0x5a5a1234f00dd00d",
    "mov rax, 0x2000",
    "int 0x80",
    "2:",
    "jmp 2b",
    "root_program_end:",
    ".previous",
);

unsafe extern "C" {
    static root_program_start: u8;
    static root_program_end: u8;
}

/// Byte slice of the embedded root program, for handoff to copy into the
/// root's USER-mapped code page. Same symbol-bounds convention as the old
/// usermode.rs `program_bytes`.
fn program_bytes() -> &'static [u8] {
    unsafe {
        let start = core::ptr::addr_of!(root_program_start) as *const u8;
        let end = core::ptr::addr_of!(root_program_end) as *const u8;
        let len = end as usize - start as usize;
        core::slice::from_raw_parts(start, len)
    }
}
