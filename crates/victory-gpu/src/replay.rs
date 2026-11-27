//! Experiment (`VICTORY_REPLAY=1`; off by default): what the title records
//! into a list is put in that list's line and done later, in order, by a
//! thread of ours; the title's call returns at once.
//!
//! The title joins its recording threads at every stage of a frame, and a
//! stage lasts as long as its slowest list: 57 ms a frame stood there, 73 %
//! of it in `draw::State::bind`, while the other recording threads and the
//! card waited. With the lines the title's stages follow one another at once
//! and the lists of several stages are prepared side by side.
//!
//! Order kept:
//! - a list's calls run in the order they were made, on one thread from the
//!   first call of a recording to its close;
//! - a queue's calls (execute, signal, wait) run in the order they were
//!   made, on one thread ([`queue`]); an execute waits until each of its
//!   lists has run everything recorded before it, and the lists do nothing
//!   further until the execute has taken their host lists;
//! - a present waits for the queue's line to be empty ([`drain`]), so the
//!   title is never more than a frame ahead.
// shortcut: what a call reads of title memory beside its arguments (the
// records of descriptor tables) is read when the call runs, up to a frame
// later than when it was made.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::object::This;

pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_REPLAY").is_some_and(|v| v == "1");
        if on {
            tracing::info!("the title's list calls go to a line per list and are done by threads of ours (VICTORY_REPLAY=1)");
        }
        on
    })
}

thread_local! {
    /// On a thread of ours that runs lines: calls are done, not put in line.
    static HERE: Cell<bool> = const { Cell::new(false) };
}

/// Whether a call made now goes to a line.
pub(crate) fn deferring() -> bool {
    on() && !HERE.get()
}

/// For the frame line: calls put in line, lines given to a thread, threads
/// started, executes, nanoseconds presents waited.
pub(crate) static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

struct Job(Box<dyn FnOnce()>);
// SAFETY: a job holds what the title's call held (addresses of its objects
// and copies of its arguments); it runs once, on one thread, in its line.
unsafe impl Send for Job {}

struct Gate {
    /// 0: not reached; 1: the line stands at it; 2: let through.
    state: Mutex<u8>,
    moved: Condvar,
}

enum Op {
    Run(Job),
    Hold(Arc<Gate>),
}

struct Ops {
    queue: VecDeque<Op>,
    /// A thread has the line.
    taken: bool,
    /// That thread is doing a call.
    busy: bool,
    /// Between a recording's first call and its close: the thread stays.
    open: bool,
}

struct Lane {
    ops: Mutex<Ops>,
    moved: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn lane(this: This) -> Arc<Lane> {
    static LANES: OnceLock<Mutex<crate::fasthash::FastMap<usize, Arc<Lane>>>> = OnceLock::new();
    thread_local!(static LAST: std::cell::RefCell<Option<(usize, Arc<Lane>)>> = const { std::cell::RefCell::new(None) });
    let key = this as usize;
    if let Some(l) = LAST.with(|l| l.borrow().as_ref().filter(|l| l.0 == key).map(|l| l.1.clone())) {
        return l;
    }
    let found = lock(LANES.get_or_init(Default::default))
        .entry(key)
        .or_insert_with(|| {
            Arc::new(Lane {
                ops: Mutex::new(Ops { queue: VecDeque::new(), taken: false, busy: false, open: true }),
                moved: Condvar::new(),
            })
        })
        .clone();
    LAST.with(|l| *l.borrow_mut() = Some((key, found.clone())));
    found
}

/// Lines waiting for a thread, and the threads waiting for a line.
static POOL: Mutex<(VecDeque<Arc<Lane>>, usize)> = Mutex::new((VecDeque::new(), 0));
static POOL_MOVED: Condvar = Condvar::new();

fn give(lane: Arc<Lane>) {
    STATS[1].fetch_add(1, Relaxed);
    let mut pool = lock(&POOL);
    pool.0.push_back(lane);
    // A thread waits for each line that waits: one of them takes it.
    if pool.1 >= pool.0.len() {
        drop(pool);
        POOL_MOVED.notify_one();
        return;
    }
    drop(pool);
    STATS[2].fetch_add(1, Relaxed);
    let made = std::thread::Builder::new().name("victory-replay".into()).stack_size(16 << 20).spawn(|| {
        HERE.set(true);
        crate::framelog::keep_on_fast_cores("victory-replay");
        loop {
            let lane = {
                let mut pool = lock(&POOL);
                loop {
                    if let Some(lane) = pool.0.pop_front() {
                        break lane;
                    }
                    pool.1 += 1;
                    pool = POOL_MOVED.wait(pool).unwrap_or_else(|e| e.into_inner());
                    pool.1 -= 1;
                }
            };
            run(&lane);
        }
    });
    if let Err(e) = made {
        tracing::error!(error = %e, "no thread for a list's line (VICTORY_REPLAY)");
    }
}

fn run(lane: &Lane) {
    loop {
        let op = {
            let mut ops = lock(&lane.ops);
            ops.busy = false;
            loop {
                if let Some(op) = ops.queue.pop_front() {
                    ops.busy = true;
                    break Some(op);
                }
                lane.moved.notify_all();
                if !ops.open {
                    ops.taken = false;
                    break None;
                }
                ops = lane.moved.wait(ops).unwrap_or_else(|e| e.into_inner());
            }
        };
        match op {
            None => return,
            Some(Op::Run(job)) => (job.0)(),
            Some(Op::Hold(gate)) => {
                let mut state = lock(&gate.state);
                *state = 1;
                gate.moved.notify_all();
                while *state != 2 {
                    state = gate.moved.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
    }
}

fn put(lane: &Arc<Lane>, op: Op, open: Option<bool>) {
    let mut ops = lock(&lane.ops);
    ops.queue.push_back(op);
    if let Some(open) = open {
        ops.open = open;
    }
    if ops.taken {
        drop(ops);
        lane.moved.notify_all();
    } else {
        ops.taken = true;
        drop(ops);
        give(lane.clone());
    }
}

/// A call of list `this`, to run in its turn.
pub(crate) fn push(this: This, job: impl FnOnce() + 'static) {
    STATS[0].fetch_add(1, Relaxed);
    put(&lane(this), Op::Run(Job(Box::new(job))), None);
}

/// [`push`] of a reset: a recording begins.
pub(crate) fn push_reset(this: This, job: impl FnOnce() + 'static) {
    put(&lane(this), Op::Run(Job(Box::new(job))), Some(true));
}

/// [`push`] of a close: the recording ends.
pub(crate) fn push_close(this: This, job: impl FnOnce() + 'static) {
    put(&lane(this), Op::Run(Job(Box::new(job))), Some(false));
}

/// Waits until list `this` has run every call made so far (before the list
/// goes away).
pub(crate) fn settle(this: This) {
    if !deferring() {
        return;
    }
    let lane = lane(this);
    let mut ops = lock(&lane.ops);
    while !ops.queue.is_empty() || ops.busy {
        ops = lane.moved.wait_timeout(ops, std::time::Duration::from_millis(2)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// The queues' line: what waits (with the gates of an execute's lists), and
/// whether its thread is at work.
type Queued = (Vec<Arc<Gate>>, Job);
static QUEUE: Mutex<(VecDeque<Queued>, bool)> = Mutex::new((VecDeque::new(), false));
static QUEUE_MOVED: Condvar = Condvar::new();

fn queue_put(gates: Vec<Arc<Gate>>, job: Job) {
    static THREAD: std::sync::Once = std::sync::Once::new();
    THREAD.call_once(|| {
        let made = std::thread::Builder::new().name("victory-replay-queue".into()).stack_size(16 << 20).spawn(|| {
            HERE.set(true);
            crate::framelog::keep_on_fast_cores("victory-replay-queue");
            loop {
                let (gates, job) = {
                    let mut q = lock(&QUEUE);
                    q.1 = false;
                    QUEUE_MOVED.notify_all();
                    loop {
                        if let Some(next) = q.0.pop_front() {
                            q.1 = true;
                            break next;
                        }
                        q = QUEUE_MOVED.wait(q).unwrap_or_else(|e| e.into_inner());
                    }
                };
                // Each list has run what was recorded before the execute.
                for gate in &gates {
                    let mut state = lock(&gate.state);
                    while *state == 0 {
                        state = gate.moved.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                }
                (job.0)();
                for gate in &gates {
                    *lock(&gate.state) = 2;
                    gate.moved.notify_all();
                }
            }
        });
        if let Err(e) = made {
            tracing::error!(error = %e, "no thread for the queues' line (VICTORY_REPLAY)");
        }
    });
    lock(&QUEUE).0.push_back((gates, job));
    QUEUE_MOVED.notify_all();
}

/// An execute of `lists`, to run in the queues' line once each list has run
/// what was recorded before it.
pub(crate) fn execute(lists: &[This], job: impl FnOnce() + 'static) {
    STATS[3].fetch_add(1, Relaxed);
    let mut seen: Vec<usize> = Vec::with_capacity(lists.len());
    let mut gates = Vec::with_capacity(lists.len());
    for &list in lists {
        if list.is_null() || seen.contains(&(list as usize)) {
            continue;
        }
        seen.push(list as usize);
        let gate = Arc::new(Gate { state: Mutex::new(0), moved: Condvar::new() });
        put(&lane(list), Op::Hold(gate.clone()), None);
        gates.push(gate);
    }
    queue_put(gates, Job(Box::new(job)));
}

/// Another call of a queue (a signal, a wait), in the queues' line.
pub(crate) fn queue(job: impl FnOnce() + 'static) {
    queue_put(Vec::new(), Job(Box::new(job)));
}

/// Waits until the queues' line is empty (a present, a call that reads what
/// the queues did).
pub(crate) fn drain() {
    if !deferring() {
        return;
    }
    let started = std::time::Instant::now();
    let mut q = lock(&QUEUE);
    while !q.0.is_empty() || q.1 {
        q = QUEUE_MOVED.wait(q).unwrap_or_else(|e| e.into_inner());
    }
    drop(q);
    STATS[4].fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
}

/// `VICTORY_REPLAY_AHEAD` (0 by default): the presents the title may be
/// ahead of the queues' line. With 1 the title records the next frame while
/// the lines of this one are still run; its presents go to the queues' line
/// too ([`present`]).
pub(crate) fn ahead() -> u64 {
    static AHEAD: OnceLock<u64> = OnceLock::new();
    *AHEAD.get_or_init(|| std::env::var("VICTORY_REPLAY_AHEAD").ok().and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// Presents in the queues' line and not done yet.
static PRESENTS: Mutex<u64> = Mutex::new(0);
static PRESENT_MOVED: Condvar = Condvar::new();

/// A present, in the queues' line; the title waits here while more than
/// [`ahead`] of them are not done.
pub(crate) fn present(job: impl FnOnce() + 'static) {
    *lock(&PRESENTS) += 1;
    queue(move || {
        job();
        *lock(&PRESENTS) -= 1;
        PRESENT_MOVED.notify_all();
    });
    let started = std::time::Instant::now();
    let mut waiting = lock(&PRESENTS);
    while *waiting > ahead() {
        waiting = PRESENT_MOVED.wait(waiting).unwrap_or_else(|e| e.into_inner());
    }
    drop(waiting);
    STATS[4].fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
}
