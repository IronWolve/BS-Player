//! Background, single-owner persistence for the player profile.
//!
//! Session snapshots are coalesced in a one-slot mailbox. Serialization,
//! atomic writes, and fsync are performed only by the worker thread.

use crate::state::{self, Session};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

enum Command {
    Save(Session),
    Finish(Session, Sender<Result<(), String>>),
}

struct Mailbox {
    pending: Option<Command>,
    accepting: bool,
    stopping: bool,
    final_result: Option<Result<(), String>>,
}

pub struct Persistence {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    results: Mutex<Receiver<Result<(), String>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    writer_loss_reported: AtomicBool,
}

type Shared = Arc<(Mutex<Mailbox>, Condvar)>;
type Results = Sender<Result<(), String>>;

impl Persistence {
    pub fn new(profile: PathBuf) -> Self {
        Self::with_worker(move |mailbox, results| worker_loop(profile, mailbox, results))
    }

    fn with_worker(run: impl FnOnce(Shared, Results) + Send + 'static) -> Self {
        let mailbox = Arc::new((
            Mutex::new(Mailbox {
                pending: None,
                accepting: true,
                stopping: false,
                final_result: None,
            }),
            Condvar::new(),
        ));
        let (results_tx, results_rx) = mpsc::channel();
        let worker_mailbox = Arc::clone(&mailbox);
        let worker = thread::Builder::new()
            .name("bs-player-profile-writer".into())
            .spawn(move || run(worker_mailbox, results_tx))
            .expect("failed to start BS Player profile writer");

        Self {
            mailbox,
            results: Mutex::new(results_rx),
            worker: Mutex::new(Some(worker)),
            writer_loss_reported: AtomicBool::new(false),
        }
    }

    fn worker_finished(&self) -> bool {
        self.worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }

    /// Queue the newest session snapshot. Any ordinary snapshot not yet picked
    /// up by the worker is replaced by this one, keeping the mailbox bounded.
    pub fn save(&self, state: Session) {
        let (lock, wake) = &*self.mailbox;
        let replaced = {
            let mut mailbox = lock.lock().unwrap_or_else(|p| p.into_inner());
            if !mailbox.accepting {
                return;
            }
            let replaced = mailbox.pending.replace(Command::Save(state));
            wake.notify_one();
            replaced
        };
        // Drop a coalesced snapshot after releasing the mailbox lock.
        drop(replaced);
    }

    /// Return the most recent completed background save result, draining any
    /// older results so the UI can report the current persistence state.
    pub fn result(&self) -> Option<Result<(), String>> {
        let receiver = self.results.lock().unwrap_or_else(|p| p.into_inner());
        let mut latest = None;
        loop {
            match receiver.try_recv() {
                Ok(result) => latest = Some(result),
                Err(TryRecvError::Empty) => return latest,
                Err(TryRecvError::Disconnected) => {
                    // The writer ended while saves were still being accepted:
                    // it died. Say so once instead of failing silently.
                    let accepting = self
                        .mailbox
                        .0
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .accepting;
                    if latest.is_none()
                        && accepting
                        && !self.writer_loss_reported.swap(true, Ordering::Relaxed)
                    {
                        latest = Some(Err(
                            "The profile writer stopped unexpectedly; progress can no longer be saved."
                                .into(),
                        ));
                    }
                    return latest;
                }
            }
        }
    }

    /// Queue a final snapshot and wait for all work ahead of it and this final
    /// write to finish. Call during normal application exit, never on the UI
    /// frame path.
    pub fn finish(&self, state: Session) -> Result<(), String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        let (lock, wake) = &*self.mailbox;
        {
            let mut mailbox = lock.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(result) = mailbox.final_result.clone() {
                return result;
            }
            if !mailbox.accepting {
                // Another caller is finishing. Wait for its final ack.
                while mailbox.final_result.is_none() {
                    mailbox = wake.wait(mailbox).unwrap_or_else(|p| p.into_inner());
                }
                return mailbox.final_result.clone().unwrap_or_else(|| {
                    Err("profile writer stopped before the final save completed".into())
                });
            }

            mailbox.accepting = false;
            mailbox.stopping = true;
            // The final snapshot supersedes any pending ordinary snapshot.
            // A save already in progress completes before the worker takes it.
            mailbox.pending = Some(Command::Finish(state, ack_tx));
            wake.notify_all();
        }

        // The reply sender also lives in the mailbox, so a dead writer never
        // disconnects the channel: poll for the reply and for the thread's end.
        let result = loop {
            match ack_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Disconnected) => {
                    break Err("profile writer stopped before acknowledging the final save".into());
                }
                Err(RecvTimeoutError::Timeout) if self.worker_finished() => {
                    break ack_rx.try_recv().unwrap_or_else(|_| {
                        Err("profile writer stopped before completing the final save".into())
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        };
        match self.join_worker() {
            Ok(()) => result,
            Err(error) => Err(error),
        }
    }

    fn join_worker(&self) -> Result<(), String> {
        let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| "profile writer thread panicked".to_owned())
    }
}

impl Drop for Persistence {
    fn drop(&mut self) {
        let (lock, wake) = &*self.mailbox;
        {
            let mut mailbox = lock.lock().unwrap_or_else(|p| p.into_inner());
            mailbox.accepting = false;
            mailbox.stopping = true;
            // Keep a pending latest snapshot; the worker writes it before exit.
            wake.notify_all();
        }
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = worker.join();
        }
    }
}

fn worker_loop(profile: PathBuf, mailbox: Shared, results: Results) {
    let mut last_saved: Option<Session> = None;
    loop {
        let command = {
            let (lock, wake) = &*mailbox;
            let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
            while state.pending.is_none() && !state.stopping {
                state = wake.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            match state.pending.take() {
                Some(command) => command,
                None => return,
            }
        };

        match command {
            Command::Save(snapshot) => {
                let result = persist_snapshot(&profile, snapshot, &mut last_saved);
                let _ = results.send(result);
            }
            Command::Finish(snapshot, ack) => {
                let result = persist_snapshot(&profile, snapshot, &mut last_saved);
                let (lock, wake) = &*mailbox;
                {
                    let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                    state.final_result = Some(result.clone());
                    state.pending = None;
                    state.stopping = true;
                    wake.notify_all();
                }
                let _ = results.send(result.clone());
                let _ = ack.send(result);
                return;
            }
        }
    }
}

fn persist_snapshot(
    profile: &std::path::Path,
    mut snapshot: Session,
    last_saved: &mut Option<Session>,
) -> Result<(), String> {
    // A non-finite number would be written as null and make session.json unreadable.
    snapshot.normalize();
    if last_saved
        .as_ref()
        .is_some_and(|previous| previous == &snapshot)
    {
        return Ok(());
    }
    state::save(profile, &snapshot)?;
    *last_saved = Some(snapshot);
    Ok(())
}
