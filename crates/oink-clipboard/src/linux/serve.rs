//! Selection serving for the X11 owner: ICCCM conversion replies, direct and
//! INCR transfers, MULTIPLE fan-out, and lazy source resolution on workers.
use crate::linux::atoms::Atoms;
use crate::linux::binding::Source;
use crate::linux::sys::Wakeup;
use crate::linux::x11::{self, Conn};
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const DIRECT_LIMIT: usize = 64 * 1024;
const CHUNK: usize = 64 * 1024;
const MAX_TRANSFERS: usize = 64;
const MAX_JOBS: usize = 32;
const MAX_MULTIPLE: usize = 256;
const TRANSFER_IDLE: Duration = Duration::from_secs(30);
const TRANSFER_TOTAL: Duration = Duration::from_secs(300);

pub(super) struct JobDone {
    generation: u64,
    requestor: u64,
    property: u64,
    target: u64,
    result: io::Result<Vec<u8>>,
}

pub(super) struct Ownership {
    pub(super) generation: u64,
    pub(super) installed: crate::linux::binding::Installed,
    pub(super) timestamp: u32,
}

pub(super) struct Transfer {
    target: u64,
    data: Vec<u8>,
    offset: usize,
    deadline: Instant,
    started: Instant,
}

/// Where one conversion answer lands: the requestor window, the property it
/// named and the target atom being answered.
#[derive(Clone, Copy)]
struct Reply {
    requestor: u64,
    property: u64,
    target: u64,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) ownership: Option<Ownership>,
    pub(super) transfers: HashMap<(u64, u64), Transfer>,
    jobs: usize,
    pub(super) generation: u64,
    pub(super) stash: Vec<x11::XEvent>,
}

pub(super) struct Context {
    pub(super) window: u64,
    /// The selection this owner thread serves (CLIPBOARD or XdndSelection).
    pub(super) selection: u64,
    pub(super) wake: Arc<Wakeup>,
    pub(super) jobs: mpsc::Sender<JobDone>,
}

pub(super) fn handle_event(conn: &Conn, atoms: &Atoms, ctx: &Context, state: &mut State, event: &x11::XEvent) {
    // SAFETY: the union is plain data; type_ is always initialized.
    let type_ = unsafe { event.type_ };
    if type_ == x11::EVENT_TYPE_SELECTION_CLEAR {
        // SAFETY: the event type selects the SelectionClear arm.
        let clear = unsafe { event.selection_clear };
        if clear.selection == ctx.selection && clear.window == ctx.window {
            release_transfers(conn, state);
            state.ownership = None;
        }
    } else if type_ == x11::EVENT_TYPE_SELECTION_REQUEST {
        // SAFETY: the event type selects the SelectionRequest arm.
        let request = unsafe { event.selection_request };
        serve(conn, atoms, ctx, state, &request);
    } else if type_ == x11::EVENT_TYPE_PROPERTY_NOTIFY {
        // SAFETY: the event type selects the PropertyNotify arm.
        let notify = unsafe { event.property };
        if notify.state == x11::EVENT_STATE_DELETE {
            advance(conn, state, notify.window, notify.atom, Instant::now());
        }
    }
}

pub(super) fn release_transfers(conn: &Conn, state: &mut State) {
    let requestors: Vec<u64> = state.transfers.keys().map(|(requestor, _)| *requestor).collect();
    state.transfers.clear();
    for requestor in requestors {
        conn.select_input(requestor, 0);
    }
}

fn serve(conn: &Conn, atoms: &Atoms, ctx: &Context, state: &mut State, request: &x11::XSelectionRequestEvent) {
    let (generation, timestamp, source) = match &state.ownership {
        None => return,
        Some(ownership) => {
            if request.selection != ctx.selection || request.owner != ctx.window {
                return;
            }
            (
                ownership.generation,
                ownership.timestamp,
                ownership.installed.source(request.target).cloned(),
            )
        }
    };
    let property = if request.property == 0 {
        request.target
    } else {
        request.property
    };
    if request.time != 0 && request.time < u64::from(timestamp) {
        refuse(conn, ctx.selection, request.requestor, request.target);
        return;
    }
    if request.target == atoms.multiple {
        serve_multiple(conn, atoms, ctx.selection, state, request, property);
        return;
    }
    match source {
        Some(Source::Bytes(bytes)) => {
            let reply = Reply {
                requestor: request.requestor,
                property,
                target: request.target,
            };
            write_target(conn, atoms, ctx.selection, state, &reply, &bytes);
        }
        Some(Source::Lazy(read)) => {
            let job = Job {
                requestor: request.requestor,
                target: request.target,
                property,
                generation,
            };
            spawn_job(conn, ctx, state, job, read);
        }
        None => control_target(conn, atoms, ctx.selection, state, request, property),
    }
}

fn control_target(
    conn: &Conn,
    atoms: &Atoms,
    selection: u64,
    state: &State,
    request: &x11::XSelectionRequestEvent,
    property: u64,
) {
    let Some(ownership) = &state.ownership else {
        return;
    };
    if request.target == atoms.timestamp {
        conn.change_property32(request.requestor, property, atoms.integer, &[ownership.timestamp]);
    } else if request.target == atoms.targets {
        conn.change_property32(
            request.requestor,
            property,
            atoms.atom_type,
            &target_list(atoms, ownership),
        );
    } else {
        refuse(conn, selection, request.requestor, request.target);
        return;
    }
    conn.send_selection_notify(request.requestor, selection, request.target, property);
}

fn target_list(atoms: &Atoms, ownership: &Ownership) -> Vec<u32> {
    // X atom ids are 32-bit even though Xlib widens them to `unsigned long`.
    let mut list = vec![atoms.targets as u32, atoms.timestamp as u32, atoms.multiple as u32];
    list.extend(ownership.installed.atoms.iter().map(|atom| *atom as u32));
    list
}

/// MULTIPLE converts byte sources only: a classic client reads every pair
/// property right after the final notify, so deferred sources and INCR sizes
/// are refused rather than starting transfers the requestor cannot follow.
fn serve_multiple(
    conn: &Conn,
    atoms: &Atoms,
    selection: u64,
    state: &mut State,
    request: &x11::XSelectionRequestEvent,
    property: u64,
) {
    let pairs = conn
        .read_pairs(request.requestor, property, MAX_MULTIPLE)
        .ok()
        .flatten()
        .unwrap_or_default();
    for (target, pair_property) in pairs {
        if pair_property == 0 || state.ownership.is_none() {
            continue;
        }
        let (timestamp, targets, source) = {
            let ownership = state.ownership.as_ref().expect("checked above");
            (
                ownership.timestamp,
                target_list(atoms, ownership),
                ownership.installed.source(target).cloned(),
            )
        };
        if target == atoms.multiple {
            conn.delete_property(request.requestor, pair_property);
        } else if target == atoms.timestamp {
            conn.change_property32(request.requestor, pair_property, atoms.integer, &[timestamp]);
        } else if target == atoms.targets {
            conn.change_property32(request.requestor, pair_property, atoms.atom_type, &targets);
        } else if let Some(Source::Bytes(bytes)) = source
            && bytes.len() <= DIRECT_LIMIT
        {
            conn.change_property(request.requestor, pair_property, target, 8, &bytes);
        } else {
            conn.delete_property(request.requestor, pair_property);
        }
    }
    conn.send_selection_notify(request.requestor, selection, atoms.multiple, property);
}

fn write_target(
    conn: &Conn,
    atoms: &Atoms,
    selection: u64,
    state: &mut State,
    reply: &Reply,
    data: &[u8],
) {
    let Reply {
        requestor,
        property,
        target,
    } = *reply;
    if data.len() <= DIRECT_LIMIT {
        conn.change_property(requestor, property, target, 8, data);
        conn.send_selection_notify(requestor, selection, target, property);
        return;
    }
    if state.transfers.len() >= MAX_TRANSFERS {
        refuse(conn, selection, requestor, target);
        return;
    }
    let now = Instant::now();
    conn.select_input(requestor, x11::PROPERTY_CHANGE_MASK);
    conn.change_property32(requestor, property, atoms.incr, &[data.len() as u32]);
    conn.send_selection_notify(requestor, selection, target, property);
    state.transfers.insert(
        (requestor, property),
        Transfer {
            target,
            data: data.to_vec(),
            offset: 0,
            deadline: now + TRANSFER_IDLE,
            started: now,
        },
    );
}

fn advance(conn: &Conn, state: &mut State, requestor: u64, property: u64, now: Instant) {
    let Some(mut transfer) = state.transfers.remove(&(requestor, property)) else {
        return;
    };
    if transfer.offset >= transfer.data.len() {
        // ICCCM ends an INCR transfer with a zero-length property.
        conn.change_property(requestor, property, transfer.target, 8, &[]);
        release_mask(conn, state, requestor);
        return;
    }
    let end = (transfer.offset + CHUNK).min(transfer.data.len());
    conn.change_property(
        requestor,
        property,
        transfer.target,
        8,
        &transfer.data[transfer.offset..end],
    );
    transfer.offset = end;
    transfer.deadline = now + TRANSFER_IDLE;
    state.transfers.insert((requestor, property), transfer);
}

fn release_mask(conn: &Conn, state: &State, requestor: u64) {
    if !state.transfers.keys().any(|(owner, _)| *owner == requestor) {
        conn.select_input(requestor, 0);
    }
}

pub(super) fn expire(conn: &Conn, state: &mut State) {
    let now = Instant::now();
    let expired: Vec<(u64, u64)> = state
        .transfers
        .iter()
        .filter(|(_, transfer)| now >= transfer.deadline || now.duration_since(transfer.started) > TRANSFER_TOTAL)
        .map(|(key, _)| *key)
        .collect();
    for key in expired {
        state.transfers.remove(&key);
        release_mask(conn, state, key.0);
    }
}

/// Time until the next transfer chunk is due, capped at the idle tick.
pub(super) fn next_tick(state: &State, idle: Duration) -> Duration {
    state
        .transfers
        .values()
        .map(|transfer| transfer.deadline.saturating_duration_since(Instant::now()))
        .min()
        .unwrap_or(idle)
        .min(idle)
}

#[derive(Clone, Copy)]
struct Job {
    requestor: u64,
    target: u64,
    property: u64,
    generation: u64,
}

fn spawn_job(
    conn: &Conn,
    ctx: &Context,
    state: &mut State,
    job: Job,
    read: Arc<dyn Fn() -> io::Result<Vec<u8>> + Send + Sync>,
) {
    let (requestor, target) = (job.requestor, job.target);
    if state.jobs >= MAX_JOBS {
        refuse(conn, ctx.selection, requestor, target);
        return;
    }
    state.jobs += 1;
    let jobs = ctx.jobs.clone();
    let wake = ctx.wake.clone();
    let spawned = std::thread::Builder::new()
        .name("oink-x11-clipboard-transfer".into())
        .spawn(move || {
            let result = read();
            let _ = jobs.send(JobDone {
                generation: job.generation,
                requestor,
                property: job.property,
                target,
                result,
            });
            wake.wake();
        });
    if spawned.is_err() {
        state.jobs -= 1;
        refuse(conn, ctx.selection, requestor, target);
    }
}

pub(super) fn finish_job(conn: &Conn, atoms: &Atoms, selection: u64, state: &mut State, done: JobDone) {
    state.jobs = state.jobs.saturating_sub(1);
    if !matches!(state.ownership.as_ref(), Some(ownership) if ownership.generation == done.generation) {
        return;
    }
    match done.result {
        Ok(data) => {
            let reply = Reply {
                requestor: done.requestor,
                property: done.property,
                target: done.target,
            };
            write_target(conn, atoms, selection, state, &reply, &data);
        }
        Err(_) => refuse(conn, selection, done.requestor, done.target),
    }
}

fn refuse(conn: &Conn, selection: u64, requestor: u64, target: u64) {
    conn.send_selection_notify(requestor, selection, target, 0);
}
