//! Tasks and the scheduler (Milestones 10 and 11).
//!
//! A task is a function with its own stack. Switching tasks ("context switch") means
//! saving the running task's registers on its stack, remembering its stack pointer,
//! and loading another task's stack pointer and registers.
//!
//! - Cooperative (M10): a task gives up the CPU by calling `yield_now()`.
//! - Preemptive (M11): the timer interrupt forces a switch on every tick.

use crate::interrupts;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::naked_asm;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

const TASK_STACK_SIZE: usize = 16 * 1024;

/// Whether the timer interrupt may switch tasks. Off = purely cooperative.
static PREEMPTION: AtomicBool = AtomicBool::new(false);

struct Task {
    name: &'static str,
    /// Saved stack pointer while the task isn't running.
    stack_pointer: u64,
    /// The task's stack memory. `None` for the boot task, which uses the bootloader's stack.
    _stack: Option<Box<[u8]>>,
    finished: bool,
}

struct Scheduler {
    current: Option<Box<Task>>,
    /// Tasks waiting for their turn, in round-robin order.
    ready: VecDeque<Box<Task>>,
    /// Finished tasks, kept until we're safely off their stacks.
    finished: Vec<Box<Task>>,
}

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler {
    current: None,
    ready: VecDeque::new(),
    finished: Vec::new(),
});

/// Turns the code that's running right now (kernel_main) into the first task.
pub fn init() {
    interrupts::without_interrupts(|| {
        SCHEDULER.lock().current = Some(Box::new(Task {
            name: "main",
            stack_pointer: 0, // filled in by the first switch away from it
            _stack: None,
            finished: false,
        }));
    });
}

/// Creates a task that will run `function` on its own stack when it gets a turn.
pub fn spawn(name: &'static str, function: impl FnOnce() + Send + 'static) {
    let mut stack = vec![0u8; TASK_STACK_SIZE].into_boxed_slice();

    // A closure can be any size, so box it (a "fat" pointer), then box that to get a
    // plain pointer we can hand to the new task in a register.
    let closure: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(function));
    let closure_pointer = Box::into_raw(closure) as u64;

    // Build the stack as if the task had called `switch_context` itself, so the first
    // switch to it "returns" into `task_trampoline`. Stacks grow downwards.
    let top = (stack.as_mut_ptr() as u64 + TASK_STACK_SIZE as u64) & !0xf; // 16-byte aligned
    let initial_frame: [u64; 7] = [
        0,                               // r15
        0,                               // r14
        0,                               // r13
        closure_pointer,                 // r12: picked up by the trampoline
        0,                               // rbx
        0,                               // rbp
        task_trampoline as *const () as u64, // return address for `ret`
    ];
    // After `ret` pops the return address, the stack pointer must be 16-byte aligned
    // (the calling convention requires it just before a `call`), hence `top - 16`.
    let stack_pointer = top - 16 - 8 * initial_frame.len() as u64;
    unsafe { (stack_pointer as *mut [u64; 7]).write(initial_frame) };

    let task = Box::new(Task { name, stack_pointer, _stack: Some(stack), finished: false });
    interrupts::without_interrupts(|| SCHEDULER.lock().ready.push_back(task));
}

/// Cooperative multitasking: let the next ready task run. Returns when it's our turn again.
pub fn yield_now() {
    // Interrupts stay off during the switch. Each task restores its own interrupt
    // state when it resumes here, because `without_interrupts` saved it on its stack.
    interrupts::without_interrupts(switch_to_next);
}

/// Preemptive multitasking: called by the timer interrupt handler (interrupts are off).
pub fn on_timer_tick() {
    if PREEMPTION.load(Ordering::Relaxed) {
        switch_to_next();
    }
}

pub fn set_preemption(enabled: bool) {
    PREEMPTION.store(enabled, Ordering::Relaxed);
}

/// Number of tasks, including the running one.
pub fn task_count() -> usize {
    interrupts::without_interrupts(|| {
        let scheduler = SCHEDULER.lock();
        scheduler.ready.len() + scheduler.current.is_some() as usize
    })
}

pub fn current_task_name() -> &'static str {
    interrupts::without_interrupts(|| SCHEDULER.lock().current.as_ref().map_or("?", |t| t.name))
}

/// Marks the running task finished and switches away for good.
fn exit_current() -> ! {
    interrupts::disable();
    if let Some(task) = SCHEDULER.lock().current.as_mut() {
        task.finished = true;
    }
    switch_to_next();
    unreachable!("a finished task was scheduled again");
}

/// Round robin: the running task goes to the back of the queue (or to `finished`),
/// and the task at the front gets the CPU. Must be called with interrupts disabled.
fn switch_to_next() {
    let switch = {
        let mut scheduler = SCHEDULER.lock();
        // Tasks that finished earlier are safe to free now: nobody runs on their stacks.
        scheduler.finished.clear();

        let Some(next) = scheduler.ready.pop_front() else {
            return; // nothing else to run: keep going with the current task
        };
        let Some(previous) = scheduler.current.take() else {
            scheduler.ready.push_front(next);
            return; // scheduler not initialised yet
        };
        let next_stack_pointer = next.stack_pointer;
        scheduler.current = Some(next);

        // Park the previous task, then take a pointer to where its stack pointer
        // should be saved. (The Box's contents never move, only the Box itself.)
        let save_slot = if previous.finished {
            scheduler.finished.push(previous);
            scheduler.finished.last_mut().unwrap()
        } else {
            scheduler.ready.push_back(previous);
            scheduler.ready.back_mut().unwrap()
        };
        (&mut save_slot.stack_pointer as *mut u64, next_stack_pointer)
        // The lock is released here, *before* switching: the next task must be able
        // to take it.
    };
    unsafe { switch_context(switch.0, switch.1) };
}

/// The context switch itself. Saves the callee-saved registers (the ones a function
/// must preserve) on the current stack, stores the stack pointer in `*save_to`, then
/// loads `load_from` as the new stack pointer and restores that task's registers.
/// The final `ret` returns into whatever the *other* task was doing.
///
/// Caller-saved registers (rax, rcx, ...) don't need saving: the compiler already
/// assumes any function call may overwrite them.
#[unsafe(naked)]
unsafe extern "C" fn switch_context(save_to: *mut u64, load_from: u64) {
    naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp", // save_to  (first argument, in rdi)
        "mov rsp, rsi",   // load_from (second argument, in rsi)
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    );
}

/// Where a new task starts: move the closure pointer (placed in r12 by `spawn`) into
/// the first-argument register and call `task_start`.
#[unsafe(naked)]
unsafe extern "C" fn task_trampoline() -> ! {
    naked_asm!("mov rdi, r12", "call {start}", "ud2", start = sym task_start);
}

extern "C" fn task_start(closure_pointer: *mut Box<dyn FnOnce() + Send>) -> ! {
    // We arrived here via a switch made with interrupts off; a fresh task wants them on.
    interrupts::enable();
    let function = unsafe { Box::from_raw(closure_pointer) };
    function();
    exit_current();
}
