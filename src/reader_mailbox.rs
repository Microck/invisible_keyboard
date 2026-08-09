//! Bounded, atomically closable handoff for blocking device readers.

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc::RecvTimeoutError},
    time::{Duration, Instant},
};

/// Create one bounded mailbox with the same sender/receiver ownership shape as
/// an MPSC channel.
///
/// An item becomes admitted only when [`MailboxSender::publish`] stores the
/// complete value in the shared queue. Closing the receiver atomically owns
/// every admitted item and wakes publishers whose reader-local item did not
/// cross that boundary.
#[must_use]
pub fn bounded_mailbox<T>(capacity: usize) -> (MailboxSender<T>, MailboxReceiver<T>) {
    assert!(capacity > 0, "reader mailbox capacity must be positive");
    let shared = Arc::new(Shared {
        capacity,
        state: Mutex::new(State {
            accepting: true,
            sender_count: 1,
            queue: VecDeque::with_capacity(capacity),
        }),
        changed: Condvar::new(),
    });
    (
        MailboxSender {
            shared: Arc::clone(&shared),
        },
        MailboxReceiver { shared },
    )
}

struct Shared<T> {
    capacity: usize,
    state: Mutex<State<T>>,
    changed: Condvar,
}

struct State<T> {
    accepting: bool,
    sender_count: usize,
    queue: VecDeque<T>,
}

/// One sequential producer for a [`MailboxReceiver`].
pub struct MailboxSender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> MailboxSender<T> {
    /// Publish one complete item, waiting for bounded queue space when needed.
    ///
    /// `true` means the receiver owns the item. `false` means closure won the
    /// admission lock and the item remained reader-local.
    pub fn publish(&self, item: T) -> bool {
        let mut state = lock(&self.shared.state);
        loop {
            if !state.accepting {
                return false;
            }
            if state.queue.len() < self.shared.capacity {
                state.queue.push_back(item);
                self.shared.changed.notify_all();
                return true;
            }
            state = wait(&self.shared, state);
        }
    }
}

impl<T> Clone for MailboxSender<T> {
    fn clone(&self) -> Self {
        let mut state = lock(&self.shared.state);
        state.sender_count = state
            .sender_count
            .checked_add(1)
            .expect("reader mailbox sender count does not overflow");
        drop(state);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for MailboxSender<T> {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.sender_count = state
            .sender_count
            .checked_sub(1)
            .expect("reader mailbox sender count does not underflow");
        drop(state);
        self.shared.changed.notify_all();
    }
}

/// The sole consumer and shutdown owner for one bounded reader mailbox.
pub struct MailboxReceiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> MailboxReceiver<T> {
    /// Receive the oldest admitted item or report a timeout/disconnection.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let started = Instant::now();
        let mut state = lock(&self.shared.state);
        loop {
            if let Some(item) = state.queue.pop_front() {
                self.shared.changed.notify_all();
                return Ok(item);
            }
            if !state.accepting || state.sender_count == 0 {
                return Err(RecvTimeoutError::Disconnected);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            let (next, timeout_result) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if timeout_result.timed_out() && state.queue.is_empty() {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    /// Close admission and take every item already owned by the mailbox.
    ///
    /// This operation serializes on the admission lock, but never waits for a
    /// blocking device read or for a reader-local item to become admitted.
    pub fn close_and_drain(&self) -> Vec<T> {
        let mut state = lock(&self.shared.state);
        state.accepting = false;
        let admitted = state.queue.drain(..).collect();
        drop(state);
        self.shared.changed.notify_all();
        admitted
    }
}

impl<T> Drop for MailboxReceiver<T> {
    fn drop(&mut self) {
        let abandoned = {
            let mut state = lock(&self.shared.state);
            state.accepting = false;
            std::mem::take(&mut state.queue)
        };
        self.shared.changed.notify_all();
        // A generic queued value can own another sender for this mailbox. Drop
        // it after unlocking so its sender destructor cannot re-enter the lock.
        drop(abandoned);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait<'a, T>(shared: &Shared<T>, state: MutexGuard<'a, State<T>>) -> MutexGuard<'a, State<T>> {
    shared
        .changed
        .wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier, mpsc},
        thread,
        time::Duration,
    };

    use super::bounded_mailbox;

    #[test]
    fn freeing_space_admits_one_waiting_item_in_order() {
        let (sender, receiver) = bounded_mailbox(1);
        assert!(sender.publish(1));
        let waiting = thread::spawn(move || sender.publish(2));

        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(1));
        assert!(waiting.join().expect("waiting publisher joins"));
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(2));
    }

    #[test]
    fn close_owns_the_full_queue_and_rejects_waiting_publishers() {
        let (sender, receiver) = bounded_mailbox(2);
        assert!(sender.publish(0));
        assert!(sender.publish(1));

        let barrier = Arc::new(Barrier::new(3));
        let (result_sender, result_receiver) = mpsc::channel();
        let publishers = [sender.clone(), sender]
            .into_iter()
            .enumerate()
            .map(|(offset, sender)| {
                let barrier = Arc::clone(&barrier);
                let result_sender = result_sender.clone();
                thread::spawn(move || {
                    barrier.wait();
                    result_sender
                        .send(sender.publish(offset + 2))
                        .expect("publisher result is observed");
                })
            })
            .collect::<Vec<_>>();
        drop(result_sender);
        barrier.wait();

        assert_eq!(receiver.close_and_drain(), vec![0, 1]);
        assert!(
            !result_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("first publisher wakes")
        );
        assert!(
            !result_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("second publisher wakes")
        );
        for publisher in publishers {
            publisher.join().expect("publisher joins");
        }
    }

    #[test]
    fn close_snapshot_equals_the_publish_result_at_the_admission_race() {
        let (sender, receiver) = bounded_mailbox(4);
        for value in 0..3 {
            assert!(sender.publish(value));
        }

        let barrier = Arc::new(Barrier::new(3));
        let (result_sender, result_receiver) = mpsc::channel();
        let publishers = [3, 4]
            .into_iter()
            .map(|value| {
                let barrier = Arc::clone(&barrier);
                let result_sender = result_sender.clone();
                let sender = sender.clone();
                thread::spawn(move || {
                    barrier.wait();
                    result_sender
                        .send((value, sender.publish(value)))
                        .expect("publisher result is observed");
                })
            })
            .collect::<Vec<_>>();
        drop(result_sender);
        drop(sender);
        barrier.wait();

        let admitted = result_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("one publisher fills the last slot");
        assert!(admitted.1);
        let drained = receiver.close_and_drain();
        let rejected = result_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("the blocked publisher loses the close race");
        assert!(!rejected.1);
        assert_eq!(&drained[..3], &[0, 1, 2]);
        assert_eq!(drained[3], admitted.0);
        assert!(!drained.contains(&rejected.0));
        for publisher in publishers {
            publisher.join().expect("publisher joins");
        }
    }

    #[test]
    fn two_publishers_keep_per_source_order_without_loss_or_duplication() {
        const VALUES_PER_SOURCE: u32 = 500;
        let (sender, receiver) = bounded_mailbox(4);
        let publishers = [0_u8, 1]
            .into_iter()
            .map(|source| {
                let sender = sender.clone();
                thread::spawn(move || {
                    for sequence in 0..VALUES_PER_SOURCE {
                        assert!(sender.publish((source, sequence)));
                    }
                })
            })
            .collect::<Vec<_>>();
        drop(sender);

        let mut observed = Vec::new();
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(value) => observed.push(value),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("active publishers stalled"),
            }
        }
        for publisher in publishers {
            publisher.join().expect("publisher joins");
        }

        assert_eq!(observed.len(), 2 * VALUES_PER_SOURCE as usize);
        for source in [0_u8, 1] {
            let sequences = observed
                .iter()
                .filter_map(|(actual_source, sequence)| {
                    (*actual_source == source).then_some(*sequence)
                })
                .collect::<Vec<_>>();
            assert_eq!(sequences, (0..VALUES_PER_SOURCE).collect::<Vec<_>>());
        }
    }

    #[test]
    fn receiver_reports_disconnect_only_after_the_admitted_backlog() {
        let (sender, receiver) = bounded_mailbox(1);
        assert!(sender.publish(7));
        drop(sender);

        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(7));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn closed_receiver_disconnects_while_senders_are_still_alive() {
        let (sender, receiver) = bounded_mailbox::<u8>(1);

        assert!(receiver.close_and_drain().is_empty());
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        assert!(!sender.publish(1));
    }

    #[test]
    fn receiver_drop_releases_the_lock_before_dropping_queued_values() {
        type ReentrantDrop = Box<dyn FnOnce() + Send>;

        let (sender, receiver) = bounded_mailbox::<ReentrantDrop>(1);
        let nested_sender = sender.clone();
        assert!(sender.publish(Box::new(move || drop(nested_sender))));

        let (done_sender, done_receiver) = mpsc::channel();
        let dropping = thread::spawn(move || {
            drop(receiver);
            done_sender
                .send(())
                .expect("receiver-drop completion is observed");
        });
        done_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("queued values cannot re-enter a held mailbox lock");
        dropping.join().expect("receiver drop joins");
    }
}
