//! The order streams with queued data are sent in.
//!
//! Streams outside any send group go first, in quinn's order: highest priority first, round
//! robin between equal priorities. Everything else is a hierarchy:
//!
//! 1. Domains share the remaining bytes by deficit round robin.
//! 2. Inside a domain, a higher-priority send group preempts every lower one.
//! 3. Send groups of equal priority share their domain's bytes by deficit round robin.
//! 4. Inside a group, streams go in their own priority order, round robin between ties.
//!
//! Both round robins charge the bytes actually written, so a group of many small streams and a
//! group of one large stream get the same rate. A turn may overdraw its credit by one frame;
//! the debt carries into the next turn, so the error stays bounded by a frame.
//!
//! Self-contained (std and slab only) so the scheduler benchmark can compile it from source.

use std::collections::{BTreeMap, VecDeque};

use slab::Slab;

/// Bytes a domain or group may send per turn of its round robin before it yields.
///
/// One minimum-size QUIC packet, so equal groups interleave about once per packet.
const QUANTUM: i64 = 1200;

/// The index of the domain every group outside an explicit domain belongs to.
const DEFAULT_DOMAIN: usize = 0;

/// A stream's place in the schedule, stored with the stream.
#[derive(Debug, Default)]
pub(super) struct Slot {
    /// The send group the stream belongs to, or `None` for the ungrouped queue.
    group: Option<usize>,
    /// Where the stream is queued, always in its own group's queue.
    queued: Option<Queued>,
}

#[derive(Debug, Clone, Copy)]
enum Queued {
    /// The stream a group resumes first, when fairness between its streams is off.
    Next,
    At(Key),
}

/// A queued stream's priority, then a decreasing counter so ties go round robin.
type Key = (i64, u64);

#[derive(Debug)]
struct Queue<K> {
    streams: BTreeMap<Key, K>,
    next: Option<K>,
}

impl<K: Copy> Queue<K> {
    fn new() -> Self {
        Self {
            streams: BTreeMap::new(),
            next: None,
        }
    }

    fn pop(&mut self) -> Option<K> {
        self.next
            .take()
            .or_else(|| self.streams.pop_last().map(|(_, id)| id))
    }

    fn remove(&mut self, queued: Queued) {
        match queued {
            Queued::Next => self.next = None,
            Queued::At(key) => {
                self.streams.remove(&key);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.next.is_none() && self.streams.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.streams.len() + self.next.is_some() as usize
    }
}

#[derive(Debug)]
struct Group<K> {
    domain: usize,
    priority: i64,
    queue: Queue<K>,
    /// Bytes this group may still send in its current turn; negative after an overdraw.
    deficit: i64,
    /// Whether the group has queued streams, or is being served one. It stays set between
    /// [`Schedule::pop`] and [`Schedule::sent`], so requeueing the stream is not a new turn.
    active: bool,
    /// Whether an entry for this group sits in its level's round robin. Usually equal to
    /// having queued streams, but a group that runs dry away from the front of the round
    /// robin keeps its entry until the round robin reaches it.
    ringed: bool,
    /// Streams that name this group, queued or not.
    members: usize,
    /// Whether the owner released the group, which is freed once no stream names it.
    closed: bool,
}

#[derive(Debug)]
struct Domain {
    /// The round robin of groups at each priority. Only the highest level with a backlogged
    /// group is served.
    levels: BTreeMap<i64, VecDeque<usize>>,
    deficit: i64,
    /// Whether an entry for this domain sits in the domain round robin, as for groups.
    ringed: bool,
    /// Groups in this domain with queued streams.
    backlogged: usize,
    /// Groups in this domain that are not yet freed.
    groups: usize,
    closed: bool,
}

impl Domain {
    fn new() -> Self {
        Self {
            levels: BTreeMap::new(),
            deficit: 0,
            ringed: false,
            backlogged: 0,
            groups: 0,
            closed: false,
        }
    }
}

/// Every stream with data to send, in the order to send it.
#[derive(Debug)]
pub(super) struct Schedule<K> {
    ungrouped: Queue<K>,
    groups: Slab<Group<K>>,
    domains: Slab<Domain>,
    /// The domain round robin.
    ring: VecDeque<usize>,
    /// Domains with a backlogged group.
    backlogged: usize,
    /// Decreases with every push, so a requeued stream goes behind its equal-priority peers.
    recency: u64,
}

impl<K: Copy> Schedule<K> {
    pub(super) fn new() -> Self {
        let mut domains = Slab::new();
        assert_eq!(domains.insert(Domain::new()), DEFAULT_DOMAIN);
        Self {
            ungrouped: Queue::new(),
            groups: Slab::new(),
            domains,
            ring: VecDeque::new(),
            backlogged: 0,
            recency: u64::MAX,
        }
    }

    /// Whether no stream is queued.
    pub(super) fn is_empty(&self) -> bool {
        self.ungrouped.is_empty() && self.backlogged == 0
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.ungrouped.len()
            + self
                .groups
                .iter()
                .map(|(_, g)| g.queue.len())
                .sum::<usize>()
    }

    /// Whether every group and domain the application released has been freed.
    #[cfg(test)]
    pub(super) fn is_clean(&self) -> bool {
        self.groups.iter().all(|(_, g)| !g.closed) && self.domains.iter().all(|(_, d)| !d.closed)
    }

    /// Create a domain, sharing bytes equally with every other domain.
    pub(super) fn open_domain(&mut self) -> usize {
        self.domains.insert(Domain::new())
    }

    /// Release a domain; it is freed once its last group is.
    pub(super) fn close_domain(&mut self, domain: usize) {
        assert_ne!(domain, DEFAULT_DOMAIN, "the default domain is never closed");
        self.domains[domain].closed = true;
        self.free_domain(domain);
    }

    /// Create a send group in `domain`, or the default domain.
    pub(super) fn open_group(&mut self, priority: i64, domain: Option<usize>) -> usize {
        let domain = domain.unwrap_or(DEFAULT_DOMAIN);
        let state = &mut self.domains[domain];
        assert!(!state.closed, "group opened in a closed domain");
        state.groups += 1;
        self.groups.insert(Group {
            domain,
            priority,
            queue: Queue::new(),
            deficit: 0,
            active: false,
            ringed: false,
            members: 0,
            closed: false,
        })
    }

    /// Release a group; it is freed once no stream names it.
    pub(super) fn close_group(&mut self, group: usize) {
        self.groups[group].closed = true;
        self.free_group(group);
    }

    pub(super) fn group_priority(&self, group: usize) -> i64 {
        self.groups[group].priority
    }

    /// Move a group to another priority level, keeping its credit.
    pub(super) fn set_group_priority(&mut self, group: usize, priority: i64) {
        let state = &mut self.groups[group];
        if state.priority == priority {
            return;
        }

        let old = std::mem::replace(&mut state.priority, priority);
        let domain = state.domain;
        if state.ringed {
            Self::unring_group(&mut self.domains[domain], group, old);
            let state = &mut self.groups[group];
            state.ringed = !state.queue.is_empty();
            if state.ringed {
                self.domains[domain]
                    .levels
                    .entry(priority)
                    .or_default()
                    .push_back(group);
            }
        }
    }

    /// Move a stream to `group`, or out of every group, keeping its place in line if queued.
    pub(super) fn join(&mut self, id: K, slot: &mut Slot, group: Option<usize>, priority: i64) {
        if slot.group == group {
            return;
        }
        if let Some(group) = group {
            assert!(!self.groups[group].closed, "stream joined a closed group");
        }

        let queued = slot.queued.is_some();
        self.remove(slot);
        self.release(slot);
        slot.group = group;
        if let Some(group) = group {
            self.groups[group].members += 1;
        }
        if queued {
            self.push(id, slot, priority);
        }
    }

    /// Forget a stream that is being freed.
    pub(super) fn leave(&mut self, slot: &mut Slot) {
        self.remove(slot);
        self.release(slot);
    }

    /// Queue a stream behind every queued stream of its group at the same priority.
    ///
    /// A stream already queued keeps its place.
    pub(super) fn push(&mut self, id: K, slot: &mut Slot, priority: i64) {
        if slot.queued.is_some() {
            return;
        }
        self.recency -= 1;
        let key = (priority, self.recency);
        slot.queued = Some(Queued::At(key));
        self.queue(slot.group).streams.insert(key, id);
        self.activate(slot.group);
    }

    /// Queue a stream ahead of every other stream of its group, to finish what it started.
    pub(super) fn reinsert(&mut self, id: K, slot: &mut Slot) {
        debug_assert!(slot.queued.is_none(), "stream queued twice");
        let queue = self.queue(slot.group);
        assert!(queue.next.is_none());
        queue.next = Some(id);
        slot.queued = Some(Queued::Next);
        self.activate(slot.group);
    }

    /// Dequeue a stream that has nothing left to send, such as one being reset.
    pub(super) fn remove(&mut self, slot: &mut Slot) {
        let Some(queued) = slot.queued.take() else {
            return;
        };
        self.queue(slot.group).remove(queued);
        self.deactivate(slot.group);
    }

    /// Take the next stream to send from.
    ///
    /// The caller writes from it and then reports back with [`Self::sent`], which requeues
    /// it and charges the bytes to its group.
    pub(super) fn pop(&mut self) -> Option<K> {
        if let Some(id) = self.ungrouped.pop() {
            return Some(id);
        }

        loop {
            let &domain = self.ring.front()?;
            let state = &mut self.domains[domain];
            if state.backlogged == 0 {
                self.ring.pop_front();
                state.ringed = false;
                continue;
            }
            if state.deficit <= 0 {
                state.deficit += QUANTUM;
                self.ring.rotate_left(1);
                continue;
            }

            let mut level = state
                .levels
                .last_entry()
                .expect("backlogged domain has a level");
            let Some(&group) = level.get().front() else {
                level.remove();
                continue;
            };
            let group_state = &mut self.groups[group];
            if !group_state.active {
                level.get_mut().pop_front();
                group_state.ringed = false;
                continue;
            }
            if group_state.deficit <= 0 {
                group_state.deficit += QUANTUM;
                level.get_mut().rotate_left(1);
                continue;
            }

            return Some(group_state.queue.pop().expect("checked non-empty"));
        }
    }

    /// Report the `bytes` written from a stream [`Self::pop`] returned, requeueing it at
    /// `priority` if it has more to send.
    ///
    /// With `fair`, it goes behind its group's equal-priority streams; without, its group
    /// resumes it before any other.
    pub(super) fn sent(
        &mut self,
        id: K,
        slot: &mut Slot,
        bytes: usize,
        requeue: Option<i64>,
        fair: bool,
    ) {
        slot.queued = None;
        if let Some(group) = slot.group {
            let bytes = bytes as i64;
            let state = &mut self.groups[group];
            state.deficit -= bytes;
            self.domains[state.domain].deficit -= bytes;
        }

        match requeue {
            Some(priority) if fair => self.push(id, slot, priority),
            Some(_) => self.reinsert(id, slot),
            None => self.deactivate(slot.group),
        }
    }

    fn queue(&mut self, group: Option<usize>) -> &mut Queue<K> {
        match group {
            Some(group) => &mut self.groups[group].queue,
            None => &mut self.ungrouped,
        }
    }

    /// Give a group that just got its first queued stream a turn.
    fn activate(&mut self, group: Option<usize>) {
        let Some(group) = group else {
            return;
        };
        let state = &mut self.groups[group];
        if state.active {
            return;
        }
        state.active = true;

        // Debt carries over, but unused credit does not, so going idle saves nothing up.
        state.deficit = state.deficit.min(0) + QUANTUM;
        let domain = &mut self.domains[state.domain];
        if !state.ringed {
            state.ringed = true;
            domain
                .levels
                .entry(state.priority)
                .or_default()
                .push_back(group);
        }

        domain.backlogged += 1;
        if domain.backlogged > 1 {
            return;
        }

        self.backlogged += 1;
        domain.deficit = domain.deficit.min(0) + QUANTUM;
        if !domain.ringed {
            domain.ringed = true;
            self.ring.push_back(self.groups[group].domain);
        }
    }

    /// Account for a group whose queue may have run dry.
    ///
    /// A group or domain heading its round robin, as one just served does, leaves it now.
    /// Anywhere else its entry stays until the round robin reaches it, since finding it
    /// would mean a scan.
    fn deactivate(&mut self, group: Option<usize>) {
        let Some(group) = group else {
            return;
        };
        let state = &mut self.groups[group];
        if !state.active || !state.queue.is_empty() {
            return;
        }
        state.active = false;

        let domain = &mut self.domains[state.domain];
        if let Some(level) = domain.levels.get_mut(&state.priority)
            && level.front() == Some(&group)
        {
            level.pop_front();
            if level.is_empty() {
                domain.levels.remove(&state.priority);
            }
            state.ringed = false;
        }

        domain.backlogged -= 1;
        if domain.backlogged > 0 {
            return;
        }

        self.backlogged -= 1;
        if self.ring.front() == Some(&state.domain) {
            self.ring.pop_front();
            domain.ringed = false;
        }
    }

    /// Drop a stream's membership of its group.
    fn release(&mut self, slot: &mut Slot) {
        if let Some(group) = slot.group.take() {
            self.groups[group].members -= 1;
            self.free_group(group);
        }
    }

    fn free_group(&mut self, group: usize) {
        let state = &self.groups[group];
        if !state.closed || state.members > 0 {
            return;
        }

        // No members means nothing queued, but a stale round-robin entry may remain.
        let state = self.groups.remove(group);
        let domain = &mut self.domains[state.domain];
        if state.ringed {
            Self::unring_group(domain, group, state.priority);
        }
        domain.groups -= 1;
        self.free_domain(state.domain);
    }

    fn free_domain(&mut self, domain: usize) {
        let state = &self.domains[domain];
        if !state.closed || state.groups > 0 {
            return;
        }

        if self.domains.remove(domain).ringed {
            let index = self.ring.iter().position(|&d| d == domain);
            self.ring
                .remove(index.expect("ringed domain is in the ring"));
        }
    }

    fn unring_group(domain: &mut Domain, group: usize, priority: i64) {
        let level = domain
            .levels
            .get_mut(&priority)
            .expect("ringed group has a level");
        let index = level.iter().position(|&g| g == group);
        level.remove(index.expect("ringed group is in its level"));
        if level.is_empty() {
            domain.levels.remove(&priority);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Streams with a fixed number of bytes each, drained a frame at a time.
    struct Sim {
        schedule: Schedule<u64>,
        streams: HashMap<u64, Stream>,
        next: u64,
        fair: bool,
    }

    struct Stream {
        slot: Slot,
        priority: i64,
        remaining: usize,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                schedule: Schedule::new(),
                streams: HashMap::new(),
                next: 0,
                fair: true,
            }
        }

        fn stream(&mut self, group: Option<usize>, priority: i64, bytes: usize) -> u64 {
            let id = self.next;
            self.next += 1;
            let mut stream = Stream {
                slot: Slot::default(),
                priority,
                remaining: bytes,
            };
            self.schedule.join(id, &mut stream.slot, group, priority);
            self.schedule.push(id, &mut stream.slot, priority);
            self.streams.insert(id, stream);
            id
        }

        /// Send one frame of up to `frame` bytes, returning the stream it came from.
        fn step(&mut self, frame: usize) -> Option<u64> {
            let id = self.schedule.pop()?;
            let stream = self.streams.get_mut(&id).unwrap();
            let bytes = frame.min(stream.remaining);
            stream.remaining -= bytes;
            let requeue = (stream.remaining > 0).then_some(stream.priority);
            self.schedule
                .sent(id, &mut stream.slot, bytes, requeue, self.fair);
            if requeue.is_none() {
                let mut stream = self.streams.remove(&id).unwrap();
                self.schedule.leave(&mut stream.slot);
            }
            Some(id)
        }
    }

    #[test]
    fn ungrouped_streams_go_first() {
        let mut sim = Sim::new();
        let group = sim.schedule.open_group(i64::MAX, None);
        let grouped = sim.stream(Some(group), i64::MAX, 100);
        let ungrouped = sim.stream(None, i64::MIN, 100);
        assert_eq!(sim.step(1000), Some(ungrouped));
        assert_eq!(sim.step(1000), Some(grouped));
        assert_eq!(sim.step(1000), None);
        assert!(sim.schedule.is_empty());
    }

    #[test]
    fn a_dry_group_away_from_the_front_is_skipped() {
        let mut sim = Sim::new();
        let a = sim.schedule.open_group(0, None);
        let b = sim.schedule.open_group(0, None);
        let first = sim.stream(Some(a), 0, 100_000);
        let second = sim.stream(Some(b), 0, 100_000);

        // `b` runs dry while `a` heads the round robin.
        let mut stream = sim.streams.remove(&second).unwrap();
        sim.schedule.leave(&mut stream.slot);
        for _ in 0..10 {
            assert_eq!(sim.step(1000), Some(first));
        }

        // And rejoins with a fresh turn.
        let second = sim.stream(Some(b), 0, 100_000);
        let mut seen = [0usize; 2];
        for _ in 0..100 {
            let id = sim.step(1000).unwrap();
            seen[(id == second) as usize] += 1;
        }
        assert!(seen[0].abs_diff(seen[1]) <= 2, "{seen:?}");
    }

    #[test]
    fn closed_groups_and_domains_free_with_their_last_stream() {
        let mut sim = Sim::new();
        let domain = sim.schedule.open_domain();
        let group = sim.schedule.open_group(0, Some(domain));
        let id = sim.stream(Some(group), 0, 10);
        sim.schedule.close_group(group);
        sim.schedule.close_domain(domain);
        assert!(!sim.schedule.is_clean());

        assert_eq!(sim.step(1000), Some(id));
        assert!(sim.schedule.is_clean());
        assert!(sim.schedule.groups.is_empty());
        assert_eq!(sim.schedule.domains.len(), 1);
        assert!(sim.schedule.ring.is_empty());
    }

    #[test]
    fn moving_a_queued_stream_keeps_it_queued() {
        let mut sim = Sim::new();
        let low = sim.schedule.open_group(0, None);
        let high = sim.schedule.open_group(1, None);
        let a = sim.stream(Some(low), 0, 10);
        let b = sim.stream(Some(low), 0, 10);

        let stream = sim.streams.get_mut(&b).unwrap();
        sim.schedule.join(b, &mut stream.slot, Some(high), 0);
        assert_eq!(sim.step(1000), Some(b));
        assert_eq!(sim.step(1000), Some(a));
        assert!(sim.schedule.is_empty());
        assert!(sim.schedule.ring.is_empty());
    }

    #[test]
    fn priority_changes_move_a_backlogged_group() {
        let mut sim = Sim::new();
        let a = sim.schedule.open_group(1, None);
        let b = sim.schedule.open_group(0, None);
        let first = sim.stream(Some(a), 0, 100_000);
        let second = sim.stream(Some(b), 0, 100_000);
        assert_eq!(sim.step(1000), Some(first));

        sim.schedule.set_group_priority(b, 2);
        assert_eq!(sim.step(1000), Some(second));
        sim.schedule.set_group_priority(a, 2);
        // Equal again, so the two alternate.
        let ids: Vec<_> = (0..4).map(|_| sim.step(1000).unwrap()).collect();
        assert!(ids.contains(&first) && ids.contains(&second), "{ids:?}");
    }
}
