//! The run loop's parking state: the one owner of every live truth the
//! durable park guard reads.
//!
//! A workflow run parks — state `parked`, engine torn down, woken later by
//! journal replay — only when nothing alive would be lost. An armed
//! in-process timer would die unfired. A spawned host call still unsettled
//! (an agent call, a child workflow, a gate evaluation) would be executed a
//! second time when the wake replays its `started` entry. A command already
//! queued for the loop — still on the channel, or handed back by an earlier
//! park attempt that storage refused — would be dispatched into a parked run
//! and replayed on the wake. These truths are loop-owned: the journal lags a
//! dispatch (an intent row is written inside the spawned task), so it must
//! never be the guard. Every arm that can park — a long timer, a decision
//! hold, an event wait — asks this model instead of reading the truths for
//! itself, and only this model closes admission, records the wake and halts
//! the engine.
//!
//! Admission is shared with the host the script talks through
//! (`ChannelWorkflowHost::send`). While a park attempt is outstanding
//! (`Parking`) commands are deferred rather than dispatched; a park that
//! lands rejects them (`Parked`); one storage refused hands them back to the
//! loop in arrival order, so the next arm to consider parking sees them as
//! queued work and refuses.

use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicU32, Ordering},
};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{HostCommand, HostCommandAdmission, HostCommandAdmissionState};

pub(super) struct RunParking {
    /// In-process timers by script-assigned id: the token cancels the armed
    /// sleep when the script abandons the wait.
    armed_timers: HashMap<u64, CancellationToken>,
    /// Spawned host work dispatched by the loop and not yet settled. Shared
    /// with the spawned tasks through their `WorkTicket`s.
    in_flight: Arc<AtomicU32>,
    /// Commands a refused park handed back, dispatched before the channel is
    /// read again.
    deferred: VecDeque<HostCommand>,
    /// The admission gate the host's `send` consults.
    admission: Arc<StdMutex<HostCommandAdmission>>,
    /// Halts the engine once a park has landed. Separate from run
    /// cancellation: a parked run is not cancelled.
    halt: CancellationToken,
    /// Set once this run has parked; later waits join it.
    parked: Option<ParkedWait>,
}

/// A run that has parked: its recorded wake, and the reply senders of every
/// wait that joined the park. Held and never resolved — the engine is being
/// torn down, and resolving a reply early would race an error into a script
/// that is still halting. The loop ends when the engine's channel closes, and
/// by then nothing awaits these.
struct ParkedWait {
    due: DateTime<Utc>,
    replies: Vec<oneshot::Sender<Result<Value, String>>>,
}

/// One unit of spawned host work. The loop takes a ticket before spawning
/// and the task settles it after its result is known — explicitly, never on
/// drop: a task that dies without settling leaves the run unparkable by
/// design, because replay must not be allowed to re-run work whose outcome
/// is unknown.
#[must_use = "settle the ticket once the work's result is known"]
pub(super) struct WorkTicket {
    in_flight: Arc<AtomicU32>,
}

impl WorkTicket {
    /// Record that the work settled. The count is published before the
    /// task replies, so a loop that reads it after the reply sees idleness.
    pub(super) fn settle(self) -> SettledWork {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        SettledWork {
            remaining: self.in_flight.load(Ordering::SeqCst),
        }
    }
}

/// The count of work still in flight at the moment a ticket settled.
pub(super) struct SettledWork {
    remaining: u32,
}

impl SettledWork {
    /// The completion hand-off: called after the task's reply is sent, so a
    /// script that reacts to the reply by asking to park finds the count
    /// already published. In tests this is where the hand-off pause hook
    /// observes it.
    pub(super) async fn hand_off(self, run_id: Uuid) {
        tracing::trace!(%run_id, remaining = self.remaining, "host work settled");
        #[cfg(test)]
        super::completion_handoff_test_support::pause(run_id, self.remaining).await;
    }
}

/// Why the guard refused a park. Each variant is one thing alive that the
/// park would lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParkRefusal {
    /// An in-process timer is armed and would die unfired.
    ArmedTimer,
    /// Spawned host work is unsettled and would be replayed.
    WorkInFlight,
    /// A command is queued for the loop — on the channel, or handed back by
    /// an earlier refused park — and would be dispatched into a parked run.
    QueuedCommand,
    /// Admission is not open: a park is already outstanding or has landed.
    AdmissionClosed,
}

/// A park the guard admitted. Admission is closed (`Parking`) from the
/// moment it is issued until it is settled, so the caller reports storage's
/// verdict through `RunParking::settle_park` before doing anything else.
#[must_use = "settle the attempt with storage's verdict"]
pub(super) struct ParkAttempt(());

impl RunParking {
    pub(super) fn new(
        admission: Arc<StdMutex<HostCommandAdmission>>,
        halt: CancellationToken,
    ) -> Self {
        Self {
            armed_timers: HashMap::new(),
            in_flight: Arc::new(AtomicU32::new(0)),
            deferred: VecDeque::new(),
            admission,
            halt,
            parked: None,
        }
    }

    /// Take a ticket for spawned work about to be dispatched.
    pub(super) fn begin_work(&self) -> WorkTicket {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        WorkTicket {
            in_flight: Arc::clone(&self.in_flight),
        }
    }

    pub(super) fn arm_timer(&mut self, timer_id: u64, abandon: CancellationToken) {
        self.armed_timers.insert(timer_id, abandon);
    }

    /// Forget an armed timer — because the script abandoned it, or because
    /// its firing was confirmed — returning its token so an abandoned sleep
    /// can be cancelled.
    pub(super) fn disarm_timer(&mut self, timer_id: u64) -> Option<CancellationToken> {
        self.armed_timers.remove(&timer_id)
    }

    /// The next command a refused park handed back, ahead of the channel.
    pub(super) fn next_deferred(&mut self) -> Option<HostCommand> {
        self.deferred.pop_front()
    }

    /// The recorded wake once this run has parked.
    pub(super) fn parked_due(&self) -> Option<DateTime<Utc>> {
        self.parked.as_ref().map(|parked| parked.due)
    }

    /// Bring a parked run's recorded wake forward. The caller has already
    /// written the earlier time; this keeps the model's copy current.
    pub(super) fn lower_wake(&mut self, due: DateTime<Utc>) {
        if let Some(parked) = self.parked.as_mut() {
            parked.due = parked.due.min(due);
        }
    }

    /// A wait arriving after the park joins it: its reply is held with the
    /// others. Returns the reply to the caller when the run is not parked,
    /// so the arm can wait in process instead.
    pub(super) fn join_park(
        &mut self,
        reply: oneshot::Sender<Result<Value, String>>,
    ) -> Result<(), oneshot::Sender<Result<Value, String>>> {
        match self.parked.as_mut() {
            Some(parked) => {
                parked.replies.push(reply);
                Ok(())
            }
            None => Err(reply),
        }
    }

    /// The park guard. Admits only when nothing alive would be lost, and
    /// then closes admission so a command sent while storage is asked is
    /// deferred rather than dispatched. `commands` is the loop's channel,
    /// read for emptiness only.
    pub(super) fn try_park(
        &mut self,
        commands: &mpsc::UnboundedReceiver<HostCommand>,
    ) -> Result<ParkAttempt, ParkRefusal> {
        if !self.armed_timers.is_empty() {
            return Err(ParkRefusal::ArmedTimer);
        }
        if self.in_flight.load(Ordering::SeqCst) != 0 {
            return Err(ParkRefusal::WorkInFlight);
        }
        if !commands.is_empty() || !self.deferred.is_empty() {
            return Err(ParkRefusal::QueuedCommand);
        }
        let mut admission = self.admission.lock().expect("host command admission");
        if !matches!(admission.state, HostCommandAdmissionState::Open) {
            return Err(ParkRefusal::AdmissionClosed);
        }
        admission.state = HostCommandAdmissionState::Parking;
        Ok(ParkAttempt(()))
    }

    /// Settle an admitted park with storage's verdict. A park that landed
    /// records its wake, rejects every command deferred meanwhile (the
    /// parked run cannot serve them) and halts the engine; the parking
    /// wait's reply is held. A park storage refused reopens admission and
    /// hands the deferred commands back to the loop, in arrival order, with
    /// the reply returned to the caller so the arm can wait in process.
    pub(super) fn settle_park(
        &mut self,
        attempt: ParkAttempt,
        landed: Option<DateTime<Utc>>,
        reply: oneshot::Sender<Result<Value, String>>,
    ) -> Option<oneshot::Sender<Result<Value, String>>> {
        let ParkAttempt(()) = attempt;
        let deferred = {
            let mut admission = self.admission.lock().expect("host command admission");
            admission.state = if landed.is_some() {
                HostCommandAdmissionState::Parked
            } else {
                HostCommandAdmissionState::Open
            };
            std::mem::take(&mut admission.deferred)
        };
        match landed {
            Some(due) => {
                deferred.into_iter().for_each(HostCommand::reject_parked);
                self.parked = Some(ParkedWait {
                    due,
                    replies: vec![reply],
                });
                self.halt.cancel();
                None
            }
            None => {
                self.deferred.extend(deferred);
                Some(reply)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::super::ChannelWorkflowHost;
    use super::*;

    fn parking() -> (
        RunParking,
        Arc<StdMutex<HostCommandAdmission>>,
        CancellationToken,
    ) {
        let admission = Arc::new(StdMutex::new(HostCommandAdmission::default()));
        let halt = CancellationToken::new();
        (
            RunParking::new(Arc::clone(&admission), halt.clone()),
            admission,
            halt,
        )
    }

    /// The script's side of the loop: a host whose `send` consults the same
    /// admission the model closes.
    fn host(
        admission: &Arc<StdMutex<HostCommandAdmission>>,
    ) -> (ChannelWorkflowHost, mpsc::UnboundedReceiver<HostCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            ChannelWorkflowHost {
                tx,
                admission: Arc::clone(admission),
                spent_tokens: Arc::new(AtomicU64::new(0)),
                max_total_tokens: None,
            },
            rx,
        )
    }

    /// Ask the guard and, when it admits, settle the attempt as refused by
    /// storage so admission is open again — the shape of a test that only
    /// cares whether the guard admits.
    fn guard_admits(
        parking: &mut RunParking,
        commands: &mpsc::UnboundedReceiver<HostCommand>,
    ) -> Result<(), ParkRefusal> {
        let attempt = parking.try_park(commands)?;
        let (reply, _receive) = oneshot::channel();
        parking
            .settle_park(attempt, None, reply)
            .expect("a refused settlement returns the reply");
        Ok(())
    }

    fn record(message: &str) -> HostCommand {
        HostCommand::Record {
            kind: "log".into(),
            message: message.into(),
        }
    }

    fn message_of(command: &HostCommand) -> &str {
        match command {
            HostCommand::Record { message, .. } => message,
            other => panic!("expected a record command, got {}", command_name(other)),
        }
    }

    fn command_name(command: &HostCommand) -> &'static str {
        match command {
            HostCommand::Record { .. } => "record",
            HostCommand::BudgetSnapshot { .. } => "budget snapshot",
            _ => "another command",
        }
    }

    #[test]
    fn an_idle_loop_may_park_and_a_command_sent_meanwhile_is_deferred() {
        let (mut parking, admission, _halt) = parking();
        let (host, commands) = host(&admission);

        let attempt = parking.try_park(&commands).expect("nothing alive refuses");
        assert!(matches!(
            admission.lock().unwrap().state,
            HostCommandAdmissionState::Parking
        ));

        host.send(record("sent while storage is asked"))
            .expect("a command sent during the attempt is deferred, not refused");
        assert!(
            commands.is_empty(),
            "a deferred command never reaches the channel"
        );
        assert_eq!(admission.lock().unwrap().deferred.len(), 1);

        let (reply, _receive) = oneshot::channel();
        parking.settle_park(attempt, None, reply);
    }

    #[test]
    fn an_armed_timer_refuses_the_park() {
        let (mut parking, admission, _halt) = parking();
        let (_host, commands) = host(&admission);
        parking.arm_timer(7, CancellationToken::new());

        assert_eq!(
            parking.try_park(&commands).err(),
            Some(ParkRefusal::ArmedTimer)
        );
        assert!(matches!(
            admission.lock().unwrap().state,
            HostCommandAdmissionState::Open
        ));

        parking.disarm_timer(7).expect("the armed token comes back");
        guard_admits(&mut parking, &commands).expect("a disarmed timer no longer refuses");
    }

    #[test]
    fn unsettled_work_refuses_the_park_until_its_ticket_settles() {
        let (mut parking, admission, _halt) = parking();
        let (_host, commands) = host(&admission);
        let first = parking.begin_work();
        let second = parking.begin_work();

        assert_eq!(
            parking.try_park(&commands).err(),
            Some(ParkRefusal::WorkInFlight)
        );
        let settled = first.settle();
        assert_eq!(settled.remaining, 1, "the count is published at settlement");
        assert_eq!(
            parking.try_park(&commands).err(),
            Some(ParkRefusal::WorkInFlight)
        );
        assert_eq!(second.settle().remaining, 0);
        guard_admits(&mut parking, &commands).expect("settled work no longer refuses");
    }

    #[test]
    fn a_command_still_on_the_channel_refuses_the_park() {
        let (mut parking, admission, _halt) = parking();
        let (host, commands) = host(&admission);
        host.send(record("queued")).expect("open admission sends");

        assert_eq!(
            parking.try_park(&commands).err(),
            Some(ParkRefusal::QueuedCommand)
        );
    }

    #[test]
    fn a_refused_park_reopens_admission_and_hands_deferred_commands_back_in_order() {
        let (mut parking, admission, halt) = parking();
        let (host, commands) = host(&admission);

        let attempt = parking.try_park(&commands).expect("idle");
        host.send(record("first")).expect("deferred");
        host.send(record("second")).expect("deferred");
        let (reply, _receive) = oneshot::channel();
        let returned = parking.settle_park(attempt, None, reply);

        assert!(
            returned.is_some(),
            "a refused park returns the wait's reply"
        );
        assert!(matches!(
            admission.lock().unwrap().state,
            HostCommandAdmissionState::Open
        ));
        assert!(!halt.is_cancelled(), "a refused park leaves the engine up");
        assert!(parking.parked_due().is_none());
        // Handed-back commands are queued work: the next park attempt
        // refuses over them until the loop has dispatched them.
        assert_eq!(
            parking.try_park(&commands).err(),
            Some(ParkRefusal::QueuedCommand)
        );
        assert_eq!(
            message_of(&parking.next_deferred().expect("first back")),
            "first"
        );
        assert_eq!(
            message_of(&parking.next_deferred().expect("second back")),
            "second"
        );
        assert!(parking.next_deferred().is_none());
        guard_admits(&mut parking, &commands).expect("the handed-back work is dispatched");
    }

    #[tokio::test]
    async fn a_landed_park_halts_the_engine_rejects_deferred_commands_and_holds_later_joins() {
        let (mut parking, admission, halt) = parking();
        let (host, commands) = host(&admission);
        let due = Utc::now() + chrono::Duration::hours(4);

        let attempt = parking.try_park(&commands).expect("idle");
        let (deferred_reply, deferred_receive) = oneshot::channel();
        host.send(HostCommand::BudgetSnapshot {
            reply: deferred_reply,
        })
        .expect("deferred during the attempt");
        let (reply, mut receive) = oneshot::channel();
        let returned = parking.settle_park(attempt, Some(due), reply);

        assert!(returned.is_none(), "the parking wait's reply is held");
        assert!(halt.is_cancelled(), "a landed park halts the engine");
        assert_eq!(parking.parked_due(), Some(due));
        assert!(matches!(
            admission.lock().unwrap().state,
            HostCommandAdmissionState::Parked
        ));
        let rejected = deferred_receive
            .await
            .expect("the deferred command is answered");
        assert_eq!(
            rejected.expect_err("a parked run serves nothing"),
            "WorkflowCancelledError: workflow run parked"
        );
        assert!(
            matches!(receive.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "the held reply is never resolved by the model"
        );

        // A later wait joins the park; its reply is held with the first.
        let (later_reply, mut later_receive) = oneshot::channel();
        parking
            .join_park(later_reply)
            .expect("a parked run takes the join");
        assert!(matches!(
            later_receive.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        // A run parked on a distant wake takes an earlier one, never a later.
        let earlier = due - chrono::Duration::hours(1);
        parking.lower_wake(due + chrono::Duration::hours(1));
        assert_eq!(parking.parked_due(), Some(due));
        parking.lower_wake(earlier);
        assert_eq!(parking.parked_due(), Some(earlier));

        assert_eq!(
            parking.try_park(&commands).err(),
            Some(ParkRefusal::AdmissionClosed),
            "a parked run never parks twice"
        );
    }

    #[test]
    fn a_join_on_a_running_run_returns_the_reply_for_an_in_process_wait() {
        let (mut parking, _admission, _halt) = parking();
        let (reply, _receive) = oneshot::channel();
        assert!(parking.join_park(reply).is_err());
    }
}
