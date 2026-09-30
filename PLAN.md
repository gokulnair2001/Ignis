# Ignis — Toy Kernel Project Plan

A hobby/learning OS kernel written in Rust, developed and tested entirely inside QEMU on macOS.

## Goals

- Learn how operating systems actually work at the hardware level: booting, memory, interrupts, scheduling.
- Build a bare-metal kernel from scratch — no existing OS underneath, runs on virtual (and eventually maybe real) x86_64 hardware.
- Long-term, ongoing project — no fixed deadline. Depth over speed.

## Approach

**Hybrid plan:** follow guided fundamentals for the first stretch (boot → memory → interrupts), then pivot to a distinctive goal once the core plumbing works — leaning toward either a **kernel-space game** (Snake/Tetris/Conway's Game of Life running directly in the kernel) or a **minimal networking stack** (enough to reply to a ping), decided later based on which feels more fun once we get there.

## Tech stack

- **Language:** Rust (`no_std`, `no_main`)
- **Target arch:** x86_64
- **Testing environment:** QEMU (`brew install qemu`) — kernel never touches real hardware or the host Mac
- **Bootloader:** `bootloader` crate (or hand-rolled Multiboot header later, if we want more control)
- **Debugging:** GDB/LLDB attached to QEMU
- **Build tooling:** `bootimage` (or `cargo-bootimage`) to produce a bootable disk image

## Milestone roadmap

### Phase 1 — Fundamentals (guided, first ~2 weeks)

| # | Milestone | What it proves |
|---|-----------|-----------------|
| 1 | `no_std` / `no_main` skeleton boots in QEMU | Toolchain + bootloader + build pipeline all work |
| 2 | Print "Hello, Ignis" via VGA text buffer | Can write to hardware memory directly |
| 3 | Serial output + basic print macro | Reliable debugging output (VGA is clunky for logs) |
| 4 | GDT (Global Descriptor Table) set up | Segmentation configured, no immediate triple faults |
| 5 | IDT (Interrupt Descriptor Table) + CPU exception handlers | Can catch page faults / GPFs instead of silent reboot |
| 6 | Hardware interrupts: PIT timer + keyboard | Kernel can react to time and input, not just run once |
| 7 | Physical memory manager (frame allocator) | Kernel can track/allocate physical RAM |
| 8 | Paging / virtual memory set up | Kernel has its own address space, higher-half kernel |
| 9 | Heap allocator (`alloc` support) | Can use `Vec`, `Box`, etc. inside the kernel |

### Phase 2 — Multitasking core

| # | Milestone | What it proves |
|---|-----------|-----------------|
| 10 | Cooperative multitasking (manual yield) | Multiple "tasks" can run without full preemption |
| 11 | Preemptive multitasking (timer-driven context switch) | Real scheduler, tasks interrupted and resumed |

### Phase 3 — Themed direction (choose one, decide after Phase 2)

**Option A — Kernel-space game**
- Render loop using VGA text mode or framebuffer
- Keyboard input wired into game logic
- Pick a game: Snake, Tetris, or Conway's Game of Life

**Option B — Minimal networking**
- NIC driver (likely via QEMU's emulated e1000 or virtio-net)
- Barebones Ethernet/IP/ICMP handling
- Goal: reply to a `ping` from the host machine

### Phase 4 — Stretch goals (optional, far future)

- Userspace support: ring 3 processes, syscall interface
- Tiny libc for userspace programs
- Basic filesystem (even a toy in-memory one)
- Boot on real hardware (spare old PC, USB stick) as a milestone checkpoint — never the main Mac

## Resources

- [Writing an OS in Rust](https://os.phil-opp.com/) — primary guided tutorial for Phase 1
- [OSDev Wiki](https://wiki.osdev.org/) — reference for everything (Multiboot, paging, ACPI, drivers)
- [OSDev Forums](https://forum.osdev.org/) — troubleshooting when stuck
- xv6 (MIT teaching OS) — reference implementation to read, not copy

## Setup checklist

- [x] Install Rust nightly + `rust-src` + `llvm-tools-preview` (custom target `x86_64-ignis.json` built via `build-std`)
- [x] `brew install qemu`
- [x] Set up `bootimage` (`cargo install bootimage`)
- [x] Confirm GDB/LLDB can attach to QEMU (LLDB + hardware breakpoint on `_start`, see Handy commands below)
- [x] First commit (covers M1–M4)

### Milestone log

- **M1 done (2026-09-29):** `no_std`/`no_main` kernel boots in QEMU via `bootloader` 0.9 and halts in `hlt_loop`. Verified with LLDB: hardware breakpoint on `_start` hits at `main.rs:9`.
- **M2 done (2026-09-29):** `vga_buffer` module (colours, volatile cell access, wrapping, scrolling), global `WRITER` behind `spin::Mutex` + `LazyLock`, `print!`/`println!` macros, panic messages shown in red.
- **M3 done (2026-09-29):** `port` module (`inb`/`outb` via inline asm), hand-written 16550 UART driver on COM1 (38400 8N1, FIFOs, polled send) with `serial_print!`/`serial_println!`; QEMU `-serial stdio` in bootimage run-args. Bonus: VGA hardware cursor follows text (CRT controller ports 0x3D4/0x3D5); `qemu::exit_qemu` via `isa-debug-exit` on port 0xF4 (Success → QEMU status 33). Panics are reported over serial too.
- **M4 done (2026-09-29):** hand-built GDT in `gdt.rs` (null, ring-0 64-bit code 0x08, data 0x10, TSS 0x18), loaded with `lgdt`, CS reloaded via far return (`retfq`), SS/DS/ES reloaded, TSS loaded with `ltr`. TSS IST[0] = 20 KiB double-fault stack, for M5. Verified in QEMU monitor: CS=0008 CS64, TR=0018 TSS64, GDT entries match, no triple fault.
- **M5 done (2026-09-29):** hand-built 256-entry IDT in `interrupts.rs` loaded with `lidt`; `extern "x86-interrupt"` handlers for #DE, #BP, #UD, #GP, #PF (reads CR2, decodes error code) and #DF on IST emergency stack. Breakpoint resumes; others report in red and halt. `crash_demo.rs` triggers each via `IGNIS_CRASH`; all 5 caught, stack overflow → double fault without reboot.
- **M6 done (2026-09-29):** 8259 PIC driver (`pic.rs`) remapped to vectors 32–47, only IRQ0/IRQ1 unmasked, EOI + spurious IRQ7/15 handling; PIT (`timer.rs`) at 100 Hz drives a status bar with uptime on row 0; PS/2 keyboard driver (`keyboard.rs`, scancode set 1, US layout, Shift/Caps Lock/Backspace) echoes typing. `without_interrupts` around print locks prevents deadlocks; `sti` enables interrupts.
- **M7 done (2026-09-30):** `frame_allocator.rs`: reads the bootloader memory map (`map_physical_memory` feature), bitmap allocator (1 bit per 4 KiB frame, up to 4 GiB) with allocate/free/reuse. QEMU default: ~120 MiB usable = 30,943 frames.
- **M8 done (2026-09-30):** `paging.rs`: hand-written 4-level page table walker (`translate`, handles 2 MiB/1 GiB huge pages) and `map_page` (creates missing tables from the frame allocator, `invlpg`). Demo maps a new virtual page onto the VGA frame. *Note:* the kernel is not higher-half — `bootloader` 0.9 loads it at its link address (0x200000); moving it high would mean relinking (code model + base address) or switching to `bootloader` 0.11. Revisit if/when userspace needs the lower half.
- **M9 done (2026-09-30):** `allocator.rs`: 1 MiB heap at 0x4444_4444_0000 mapped page by page; hand-written linked-list allocator (address-sorted free list, first fit, 16-byte granules, merges neighbours) registered as `#[global_allocator]`; `alloc` added to build-std. Self-tests: Box/Vec/String, 10 MiB churn through 1 MiB, 900 KiB after merging, no leaks. `IGNIS_CRASH=oom` shows allocation failure → panic.
- **M10 done (2026-09-30):** `scheduler.rs`: tasks with their own 16 KiB heap-allocated stacks, naked-asm `switch_context` (saves callee-saved regs + RSP), trampoline to start closures, round-robin ready queue, `yield_now`, finished tasks freed once off their stack. Demo: A1 B1 C1 A2 B2 C2 A3 B3 C3.
- **M11 done (2026-09-30):** timer IRQ calls `scheduler::on_timer_tick()` after EOI, switching tasks 100×/s when preemption is on. Demo: three never-yielding tasks with 1×/2×/3× work share the CPU evenly (progress bars on row 1; finish times follow a 4:7:9 ratio because the idle main task also takes a turn). *Known limits:* no idle-task special case, no sleeping/blocking, no stack guard pages for task stacks.

### Handy commands

- Build image: `cargo bootimage`
- Build + run in QEMU: `cargo run`
- Crash demos: `IGNIS_CRASH=<divide|opcode|gpf|page_fault|stack_overflow|oom> cargo run`
- Debug: `qemu-system-x86_64 -drive format=raw,file=target/x86_64-ignis/debug/bootimage-ignis.bin -s -S` then
  `lldb target/x86_64-ignis/debug/ignis` → `gdb-remote 1234` → `breakpoint set -H -n _start` → `continue`

## Notes

- Everything runs inside QEMU as a normal macOS userspace process — no risk to the host Mac. Real hardware testing (if we ever do it) uses a separate spare machine.
- This doc is the single source of truth for scope — update it as milestones are completed or the plan changes.
