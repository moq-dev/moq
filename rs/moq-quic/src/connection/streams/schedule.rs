//! The order streams and datagrams with queued data are sent in.
//!
//! Streams and datagrams outside any send group go first, in quinn's order: highest priority
//! first, round robin between equal priorities. Everything else is a hierarchy:
//!
//! 1. Domains share the remaining bytes by deficit round robin.
//! 2. Inside a domain, a higher-priority send group preempts every lower one.
//! 3. Send groups of equal priority share their domain's bytes by deficit round robin.
//! 4. Inside a group, streams and datagrams go in their own order, round robin between tied
//!    streams. A datagram goes ahead of a stream of equal order, and tied datagrams oldest
//!    first.
//!
//! Both round robins charge the bytes actually written, so a group of many small streams and a
//! group of one large stream get the same rate, and a group's datagrams spend the same credit
//! as its streams. A turn may overdraw its credit by one frame; the debt carries into the next
//! turn, so the error stays bounded by a frame.
//!
//! Queued datagrams share one byte budget. When it overflows, the domain holding the most
//! datagram bytes gives up the oldest datagram of its lowest-priority group. Priorities are
//! only compared inside a domain, so one busy domain cannot evict another's datagrams.
//!
//! Self-contained (std and slab only) so the scheduler benchmark can compile it from source.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, VecDeque},
};

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

/// A queued item's order, then whether it is a datagram, which wins a tie, then a decreasing
/// counter so the rest of a tie goes oldest first: round robin for requeued streams.
type Key = (i64, bool, u64);

/// What to send next, from [`Schedule::pop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Next<K> {
    /// A stream, out of the schedule until [`Schedule::sent`] reports back.
    Stream(K),
    /// The datagram heading a group, or the ungrouped queue with `None`. It stays queued
    /// until [`Schedule::sent_datagram`] takes it.
    Datagram(Option<usize>),
}

#[derive(Debug)]
enum Item<K, D> {
    Stream(K),
    /// A datagram and the bytes of its frame, which it holds against the budget.
    Datagram(D, usize),
}

#[derive(Debug)]
struct Queue<K, D> {
    items: BTreeMap<Key, Item<K, D>>,
    next: Option<K>,
    /// The order of every queued datagram by its counter, so the oldest is the last.
    datagrams: BTreeMap<u64, i64>,
}

impl<K: Copy, D> Queue<K, D> {
    fn new() -> Self {
        Self {
            items: BTreeMap::new(),
            next: None,
            datagrams: BTreeMap::new(),
        }
    }

    fn pop(&mut self, group: Option<usize>) -> Option<Next<K>> {
        if let Some(id) = self.next.take() {
            return Some(Next::Stream(id));
        }
        let head = self.items.last_entry()?;
        if let Item::Datagram(..) = head.get() {
            return Some(Next::Datagram(group));
        }
        let Item::Stream(id) = head.remove() else {
            unreachable!("checked for a stream");
        };
        Some(Next::Stream(id))
    }

    fn remove(&mut self, queued: Queued) {
        match queued {
            Queued::Next => self.next = None,
            Queued::At(key) => {
                self.items.remove(&key);
            }
        }
    }

    /// The datagram heading the queue, its frame bytes, and its counter.
    fn head_datagram(&self) -> (&D, usize, u64) {
        match self.items.last_key_value() {
            Some((&(_, _, recency), Item::Datagram(datagram, frame))) => {
                (datagram, *frame, recency)
            }
            _ => panic!("no datagram heads the queue"),
        }
    }

    /// The counter of the oldest queued datagram.
    fn oldest_datagram(&self) -> Option<u64> {
        self.datagrams.last_key_value().map(|(&recency, _)| recency)
    }

    fn take_datagram(&mut self, recency: u64) -> (D, usize) {
        let order = self.datagrams.remove(&recency).expect("datagram is queued");
        match self.items.remove(&(order, true, recency)) {
            Some(Item::Datagram(datagram, size)) => (datagram, size),
            _ => unreachable!("an indexed datagram is queued"),
        }
    }

    fn is_empty(&self) -> bool {
        self.next.is_none() && self.items.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.items.len() + self.next.is_some() as usize
    }
}

#[derive(Debug)]
struct Group<K, D> {
    domain: usize,
    priority: i64,
    queue: Queue<K, D>,
    /// Bytes this group may still send in its current turn; negative after an overdraw.
    deficit: i64,
    /// Whether the group has queued items, or is being served a stream. It stays set between
    /// [`Schedule::pop`] and [`Schedule::sent`], so requeueing the stream is not a new turn.
    active: bool,
    /// Whether an entry for this group sits in its level's round robin. Usually equal to
    /// having queued items, but a group that runs dry away from the front of the round
    /// robin keeps its entry until the round robin reaches it.
    ringed: bool,
    /// Streams that name this group, queued or not.
    members: usize,
    /// Whether the owner released the group, which is freed once no stream names it and its
    /// datagrams are gone.
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
    /// Groups in this domain with queued items.
    backlogged: usize,
    /// Groups in this domain that are not yet freed.
    groups: usize,
    closed: bool,
    /// Datagram bytes queued in the domain.
    datagram_bytes: usize,
    /// The counter of every datagram queued in the domain, so the last is its oldest.
    datagrams: BTreeSet<u64>,
    /// The order the domain's queues give up datagrams in: groups before the ungrouped queue
    /// (`None`, in the default domain), lowest priority first, then the oldest datagram.
    victims: BTreeSet<Victim>,
}

/// A queue holding datagrams, keyed for eviction: whether it is the ungrouped queue, its
/// group's priority, its oldest datagram, and the group.
type Victim = (bool, i64, Reverse<u64>, Option<usize>);

impl Domain {
    fn new() -> Self {
        Self {
            levels: BTreeMap::new(),
            deficit: 0,
            ringed: false,
            backlogged: 0,
            groups: 0,
            closed: false,
            datagram_bytes: 0,
            datagrams: BTreeSet::new(),
            victims: BTreeSet::new(),
        }
    }
}

/// Every stream and datagram with data to send, in the order to send it.
#[derive(Debug)]
pub(super) struct Schedule<K, D> {
    ungrouped: Queue<K, D>,
    groups: Slab<Group<K, D>>,
    domains: Slab<Domain>,
    /// The domain round robin.
    ring: VecDeque<usize>,
    /// Domains with a backlogged group.
    backlogged: usize,
    /// Decreases with every push, so a requeued stream goes behind its equal-priority peers.
    recency: u64,
    /// Domains holding datagrams, by their datagram bytes then their oldest datagram, so the
    /// last gives one up first.
    hoards: BTreeSet<(usize, u64, usize)>,
    /// Datagram bytes queued across every domain, held against the budget.
    datagram_bytes: usize,
    /// How many queued datagrams have each frame size, so the smallest is the first.
    frames: BTreeMap<usize, usize>,
    /// Streams queued, not counting one between [`Self::pop`] and [`Self::sent`].
    streams: usize,
    /// Datagrams [`Self::defer_datagram`] took out of line until [`Self::restore`].
    deferred: Vec<(Option<usize>, Key, Item<K, D>)>,
}

impl<K: Copy, D> Schedule<K, D> {
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
            hoards: BTreeSet::new(),
            datagram_bytes: 0,
            frames: BTreeMap::new(),
            streams: 0,
            deferred: Vec::new(),
        }
    }

    /// Whether nothing is queued.
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.ungrouped.is_empty() && self.backlogged == 0
    }

    /// Whether a packet with room for a `space`-byte frame would carry something: any stream,
    /// which can be split to fit, or a datagram no larger than that.
    pub(super) fn can_send(&self, space: usize) -> bool {
        self.streams > 0
            || self
                .frames
                .first_key_value()
                .is_some_and(|(&frame, _)| frame <= space)
    }

    /// Frame bytes held by queued datagrams.
    pub(super) fn datagram_bytes(&self) -> usize {
        self.datagram_bytes
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
    #[cfg(test)]
    pub(super) fn open_domain(&mut self) -> usize {
        self.domains.insert(Domain::new())
    }

    /// Release a domain; it is freed once its last group is.
    #[cfg(test)]
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

    /// Release a group; it is freed once no stream names it and its datagrams are gone.
    pub(super) fn close_group(&mut self, group: usize) {
        self.groups[group].closed = true;
        self.free_group(group);
    }

    pub(super) fn group_priority(&self, group: usize) -> i64 {
        self.groups[group].priority
    }

    /// Move a group to another priority level, keeping its credit.
    pub(super) fn set_group_priority(&mut self, group: usize, priority: i64) {
        if self.groups[group].priority == priority {
            return;
        }

        self.unindex(Some(group));
        let state = &mut self.groups[group];
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
        self.index(Some(group));
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
        let key = (priority, false, self.recency);
        slot.queued = Some(Queued::At(key));
        self.queue(slot.group).items.insert(key, Item::Stream(id));
        self.streams += 1;
        self.activate(slot.group);
    }

    /// Queue a stream ahead of every other stream of its group, to finish what it started.
    pub(super) fn reinsert(&mut self, id: K, slot: &mut Slot) {
        debug_assert!(slot.queued.is_none(), "stream queued twice");
        let queue = self.queue(slot.group);
        assert!(queue.next.is_none());
        queue.next = Some(id);
        slot.queued = Some(Queued::Next);
        self.streams += 1;
        self.activate(slot.group);
    }

    /// Dequeue a stream that has nothing left to send, such as one being reset.
    pub(super) fn remove(&mut self, slot: &mut Slot) {
        let Some(queued) = slot.queued.take() else {
            return;
        };
        self.queue(slot.group).remove(queued);
        self.streams -= 1;
        self.deactivate(slot.group);
    }

    /// Queue a datagram in `group`, or ungrouped, at `order` among that queue's streams and
    /// datagrams. Its `frame` bytes are what it sends and holds of the `budget` every queued
    /// datagram shares.
    ///
    /// Returns whether it stayed queued: while the budget overflows, the domain holding the
    /// most datagram bytes evicts the oldest datagram of its lowest-priority group, and this
    /// datagram is dropped once it is the one to go.
    pub(super) fn push_datagram(
        &mut self,
        group: Option<usize>,
        order: i64,
        datagram: D,
        frame: usize,
        budget: usize,
    ) -> bool {
        if let Some(group) = group {
            assert!(
                !self.groups[group].closed,
                "datagram sent on a closed group"
            );
        }

        self.recency -= 1;
        let recency = self.recency;
        self.unindex(group);
        let queue = self.queue(group);
        queue
            .items
            .insert((order, true, recency), Item::Datagram(datagram, frame));
        queue.datagrams.insert(recency, order);
        let domain = self.domain_of(group);
        let domain = &mut self.domains[domain];
        domain.datagram_bytes += frame;
        domain.datagrams.insert(recency);
        self.datagram_bytes += frame;
        *self.frames.entry(frame).or_default() += 1;
        self.index(group);
        self.activate(group);

        while self.datagram_bytes > budget {
            let &(_, _, domain) = self.hoards.last().expect("queued bytes are indexed");
            let &(_, _, Reverse(oldest), victim) = self.domains[domain]
                .victims
                .first()
                .expect("a domain holding datagram bytes has a victim");
            self.remove_datagram(victim, oldest);
            if oldest == recency {
                return false;
            }
        }
        true
    }

    /// The datagram [`Self::pop`] returned as [`Next::Datagram`], and its frame bytes.
    pub(super) fn datagram(&self, group: Option<usize>) -> (&D, usize) {
        let (datagram, frame, _) = self.head(group);
        (datagram, frame)
    }

    /// Take the datagram [`Self::pop`] returned as [`Next::Datagram`], charging its frame to
    /// its group.
    pub(super) fn sent_datagram(&mut self, group: Option<usize>) -> D {
        let (_, frame, recency) = self.head(group);
        if let Some(group) = group {
            let state = &mut self.groups[group];
            state.deficit -= frame as i64;
            self.domains[state.domain].deficit -= frame as i64;
        }
        self.remove_datagram(group, recency)
    }

    /// Take the datagram [`Self::pop`] returned out of line, so what queues behind it can fill
    /// a packet it does not fit. [`Self::restore`] puts it back in its place.
    ///
    /// A group this empties restarts its turn when restored, gaining up to a quantum of credit,
    /// so this is for the rare packet clamped below the path MTU, not every full one.
    pub(super) fn defer_datagram(&mut self, group: Option<usize>) {
        let (key, item) = self
            .queue(group)
            .items
            .pop_last()
            .expect("a datagram heads");
        self.deferred.push((group, key, item));
        // Its group stays allocated: the datagram is still indexed for eviction.
        self.deactivate(group);
    }

    /// Requeue every datagram [`Self::defer_datagram`] took out of line.
    pub(super) fn restore(&mut self) {
        for (group, key, item) in std::mem::take(&mut self.deferred) {
            self.queue(group).items.insert(key, item);
            self.activate(group);
        }
    }

    fn head(&self, group: Option<usize>) -> (&D, usize, u64) {
        match group {
            Some(group) => self.groups[group].queue.head_datagram(),
            None => self.ungrouped.head_datagram(),
        }
    }

    /// Drop every queued datagram `keep` refuses, returning whether any was.
    pub(super) fn retain_datagrams(&mut self, mut keep: impl FnMut(&D) -> bool) -> bool {
        let groups = self.groups.iter().map(|(index, g)| (Some(index), &g.queue));
        let mut doomed = Vec::new();
        for (group, queue) in std::iter::once((None, &self.ungrouped)).chain(groups) {
            for (&recency, &order) in &queue.datagrams {
                if let Some(Item::Datagram(datagram, _)) = queue.items.get(&(order, true, recency))
                    && !keep(datagram)
                {
                    doomed.push((group, recency));
                }
            }
        }
        for &(group, recency) in &doomed {
            self.remove_datagram(group, recency);
        }
        !doomed.is_empty()
    }

    /// Take the next stream or datagram to send.
    ///
    /// The caller writes from a stream and then reports back with [`Self::sent`], which
    /// requeues it and charges the bytes to its group. A datagram stays queued until
    /// [`Self::sent_datagram`] takes it, so one that does not fit waits for the next packet.
    pub(super) fn pop(&mut self) -> Option<Next<K>> {
        let next = self.pop_next();
        if let Some(Next::Stream(_)) = next {
            self.streams -= 1;
        }
        next
    }

    fn pop_next(&mut self) -> Option<Next<K>> {
        if let Some(next) = self.ungrouped.pop(None) {
            return Some(next);
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

            return Some(
                group_state
                    .queue
                    .pop(Some(group))
                    .expect("checked non-empty"),
            );
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

    fn queue(&mut self, group: Option<usize>) -> &mut Queue<K, D> {
        match group {
            Some(group) => &mut self.groups[group].queue,
            None => &mut self.ungrouped,
        }
    }

    fn domain_of(&self, group: Option<usize>) -> usize {
        group.map_or(DEFAULT_DOMAIN, |group| self.groups[group].domain)
    }

    /// Dequeue a datagram, leaving the round robin with the group's last item.
    fn remove_datagram(&mut self, group: Option<usize>, recency: u64) -> D {
        self.unindex(group);
        let (datagram, frame) = self.queue(group).take_datagram(recency);
        let domain = self.domain_of(group);
        let domain = &mut self.domains[domain];
        domain.datagram_bytes -= frame;
        domain.datagrams.remove(&recency);
        self.datagram_bytes -= frame;
        if let std::collections::btree_map::Entry::Occupied(mut count) = self.frames.entry(frame) {
            *count.get_mut() -= 1;
            if *count.get() == 0 {
                count.remove();
            }
        }
        self.index(group);

        self.deactivate(group);
        if let Some(group) = group {
            self.free_group(group);
        }
        datagram
    }

    /// Withdraw a queue and its domain from the eviction order, ahead of changing its
    /// datagrams or priority; [`Self::index`] puts them back.
    fn unindex(&mut self, group: Option<usize>) {
        self.reindex(group, false);
    }

    fn index(&mut self, group: Option<usize>) {
        self.reindex(group, true);
    }

    fn reindex(&mut self, group: Option<usize>, insert: bool) {
        let (queue, priority) = match group {
            Some(index) => {
                let state = &self.groups[index];
                (&state.queue, state.priority)
            }
            None => (&self.ungrouped, i64::MAX),
        };
        let index = self.domain_of(group);
        let domain = &mut self.domains[index];
        if let Some(oldest) = queue.oldest_datagram() {
            let victim = (group.is_none(), priority, Reverse(oldest), group);
            match insert {
                true => domain.victims.insert(victim),
                false => domain.victims.remove(&victim),
            };
        }
        if let Some(&oldest) = domain.datagrams.last() {
            let hoard = (domain.datagram_bytes, oldest, index);
            match insert {
                true => self.hoards.insert(hoard),
                false => self.hoards.remove(&hoard),
            };
        }
    }

    /// Give a group that just got its first queued item a turn.
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
        if !state.closed || state.members > 0 || !state.queue.datagrams.is_empty() {
            return;
        }

        // Nothing names or fills the group, but a stale round-robin entry may remain.
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

    /// Streams with a fixed number of bytes each, drained a frame at a time, and datagrams
    /// sent whole. Datagrams carry their id and size.
    struct Sim {
        schedule: Schedule<u64, (u64, usize)>,
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

        /// Queue a datagram of `bytes` within `budget`, returning its id and whether it stayed.
        fn datagram(
            &mut self,
            group: Option<usize>,
            order: i64,
            bytes: usize,
            budget: usize,
        ) -> (u64, bool) {
            let id = self.next;
            self.next += 1;
            let queued = self
                .schedule
                .push_datagram(group, order, (id, bytes), bytes, budget);
            (id, queued)
        }

        /// Send one frame of up to `frame` bytes or one datagram, returning its id.
        fn step(&mut self, frame: usize) -> Option<u64> {
            let id = match self.schedule.pop()? {
                Next::Stream(id) => id,
                Next::Datagram(group) => {
                    return Some(self.schedule.sent_datagram(group).0);
                }
            };
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

        /// Send everything left, returning the ids in order.
        fn drain(&mut self) -> Vec<u64> {
            std::iter::from_fn(|| self.step(1000)).collect()
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

    #[test]
    fn datagrams_follow_their_order_and_win_a_tie() {
        let mut sim = Sim::new();
        let group = sim.schedule.open_group(0, None);
        let older = sim.datagram(Some(group), 10, 100, usize::MAX).0;
        let stream = sim.stream(Some(group), 11, 100);
        let tied = sim.stream(Some(group), 10, 100);
        let first = sim.datagram(Some(group), 10, 100, usize::MAX).0;
        let second = sim.datagram(Some(group), 10, 100, usize::MAX).0;

        // The newer group first, then its tie: datagrams oldest first, ahead of the stream.
        assert_eq!(sim.drain(), [stream, older, first, second, tied]);
        assert!(sim.schedule.is_empty());
        assert_eq!(sim.schedule.datagram_bytes(), 0);
    }

    #[test]
    fn a_closed_group_sends_its_datagrams_then_frees() {
        let mut sim = Sim::new();
        let group = sim.schedule.open_group(0, None);
        let id = sim.datagram(Some(group), 0, 100, usize::MAX).0;
        sim.schedule.close_group(group);
        assert!(!sim.schedule.is_clean());

        assert_eq!(sim.drain(), [id]);
        assert!(sim.schedule.is_clean());
        assert!(sim.schedule.groups.is_empty());
    }

    #[test]
    fn eviction_takes_the_lowest_priority_oldest_datagram() {
        let mut sim = Sim::new();
        let high = sim.schedule.open_group(1, None);
        let low = sim.schedule.open_group(0, None);
        let low_old = sim.datagram(Some(low), 0, 100, 400).0;
        let high_old = sim.datagram(Some(high), 0, 100, 400).0;
        // Newer but higher order: eviction goes by age, not order.
        let low_new = sim.datagram(Some(low), 5, 100, 400).0;
        let ungrouped = sim.datagram(None, 0, 100, 400).0;

        // Full: the next evicts the low group's oldest, then its newest.
        let (high_new, queued) = sim.datagram(Some(high), 0, 100, 400);
        assert!(queued);
        assert!(sim.datagram(Some(high), 0, 100, 400).1);
        assert_eq!(sim.drain(), [ungrouped, high_old, high_new, high_new + 1]);
        let _ = (low_old, low_new);
    }

    #[test]
    fn the_new_datagram_drops_when_it_is_the_victim() {
        let mut sim = Sim::new();
        let high = sim.schedule.open_group(1, None);
        let low = sim.schedule.open_group(0, None);
        let kept = sim.datagram(Some(high), 0, 100, 100).0;
        assert!(!sim.datagram(Some(low), 0, 100, 100).1);
        assert_eq!(sim.schedule.datagram_bytes(), 100);
        assert!(sim.schedule.is_clean());
        assert_eq!(sim.drain(), [kept]);
    }

    #[test]
    fn eviction_stays_inside_the_busiest_domain() {
        let mut sim = Sim::new();
        let quiet = sim.schedule.open_domain();
        let busy = sim.schedule.open_domain();
        let quiet_low = sim.schedule.open_group(0, Some(quiet));
        let busy_high = sim.schedule.open_group(9, Some(busy));

        // The quiet domain's low-priority datagram survives a busy high-priority domain.
        let survivor = sim.datagram(Some(quiet_low), 0, 100, 500).0;
        let mut busy_ids = Vec::new();
        for _ in 0..10 {
            busy_ids.push(sim.datagram(Some(busy_high), 0, 100, 500).0);
        }
        let sent = sim.drain();
        assert!(sent.contains(&survivor), "{sent:?}");
        // The busy domain kept its newest four.
        assert_eq!(sent.len(), 5);
        assert!(busy_ids[6..].iter().all(|id| sent.contains(id)), "{sent:?}");
    }

    #[test]
    fn domains_tied_on_bytes_evict_from_the_older() {
        let mut sim = Sim::new();
        let groups: Vec<_> = (0..3)
            .map(|_| {
                let domain = sim.schedule.open_domain();
                sim.schedule.open_group(0, Some(domain))
            })
            .collect();
        let ids: Vec<_> = groups
            .iter()
            .map(|&group| sim.datagram(Some(group), 0, 100, 250))
            .collect();
        // Each domain holds 100 bytes, so the one with the oldest datagram gives it up.
        assert!(ids.iter().all(|&(_, queued)| queued));
        let sent = sim.drain();
        assert!(!sent.contains(&ids[0].0), "{sent:?}");
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn a_deferred_datagram_keeps_its_place() {
        let mut sim = Sim::new();
        let group = sim.schedule.open_group(0, None);
        let large = sim.datagram(Some(group), 2, 1000, usize::MAX).0;
        let small = sim.datagram(Some(group), 1, 10, usize::MAX).0;
        let stream = sim.stream(Some(group), 0, 100);

        // Only 100 bytes of room: the large datagram steps aside for what fits behind it.
        assert_eq!(sim.schedule.pop(), Some(Next::Datagram(Some(group))));
        sim.schedule.defer_datagram(Some(group));
        assert_eq!(sim.step(1000), Some(small));
        assert_eq!(sim.step(1000), Some(stream));
        assert_eq!(sim.step(1000), None);
        sim.schedule.restore();
        assert!(!sim.schedule.can_send(999));
        assert!(sim.schedule.can_send(1000));
        assert_eq!(sim.drain(), [large]);
    }

    #[test]
    fn retained_datagrams_drop_what_no_longer_fits() {
        let mut sim = Sim::new();
        let group = sim.schedule.open_group(0, None);
        let small = sim.datagram(Some(group), 0, 10, usize::MAX).0;
        sim.datagram(Some(group), 0, 1000, usize::MAX);
        sim.datagram(None, 0, 1000, usize::MAX);
        assert!(sim.schedule.retain_datagrams(|&(_, bytes)| bytes <= 100));
        assert!(!sim.schedule.retain_datagrams(|&(_, bytes)| bytes <= 100));
        assert_eq!(sim.schedule.datagram_bytes(), 10);
        assert_eq!(sim.drain(), [small]);
    }
}
