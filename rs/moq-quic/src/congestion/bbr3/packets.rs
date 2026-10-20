//! Packet-number lookup and incremental retirement for BBR's delivery snapshots.

#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};

use super::{BbrPacket, ROUND_COUNT_WINDOW, SpaceId};

#[derive(Debug, Clone, Default)]
pub(super) struct Packets {
    spaces: [BTreeMap<u64, BbrPacket>; 3],
    expiry: BTreeSet<(u64, usize, u64)>,
    acknowledged: Vec<(usize, u64)>,
    stale: Vec<(usize, u64)>,
    // The unchanged queue bookkeeping is a policy and scaling reference in tests only.
    #[cfg(test)]
    scan: Option<[VecDeque<BbrPacket>; 3]>,
}

impl Packets {
    pub(super) fn insert(&mut self, packet: BbrPacket) {
        #[cfg(test)]
        if let Some(scan) = &mut self.scan {
            scan[packet.space as usize].push_back(packet);
            return;
        }
        let space = packet.space as usize;
        let pn = packet.packet_number;
        assert!(
            self.spaces[space].insert(pn, packet).is_none(),
            "a packet number was reused within its space"
        );
        self.expiry.insert((packet.round_count, space, pn));
    }

    pub(super) fn acknowledge(&mut self, space: SpaceId, pn: u64) -> Option<BbrPacket> {
        let space = space as usize;
        #[cfg(test)]
        if let Some(scan) = &mut self.scan {
            let index = scan[space]
                .binary_search_by_key(&pn, |packet| packet.packet_number)
                .ok()?;
            scan[space][index].acknowledged = true;
            return Some(scan[space][index]);
        }
        let packet = self.spaces[space].get_mut(&pn)?;
        if !packet.acknowledged {
            packet.acknowledged = true;
            self.acknowledged.push((space, pn));
        }
        Some(*packet)
    }

    pub(super) fn remove(&mut self, space: SpaceId, pn: u64) -> Option<BbrPacket> {
        self.remove_at(space as usize, pn)
    }

    fn remove_at(&mut self, space: usize, pn: u64) -> Option<BbrPacket> {
        #[cfg(test)]
        if let Some(scan) = &mut self.scan {
            let index = scan[space]
                .binary_search_by_key(&pn, |packet| packet.packet_number)
                .ok()?;
            return scan[space].remove(index);
        }
        let packet = self.spaces[space].remove(&pn)?;
        self.expiry.remove(&(packet.round_count, space, pn));
        Some(packet)
    }

    pub(super) fn cleanup(&mut self, round: u64) {
        #[cfg(test)]
        if let Some(scan) = &mut self.scan {
            for packets in scan {
                packets.retain(|packet| !packet.stale);
                for packet in packets {
                    if packet.acknowledged || round - packet.round_count > ROUND_COUNT_WINDOW {
                        packet.stale = true;
                    }
                }
            }
            return;
        }
        // Keep completed snapshots through one further ACK batch: a late loss may
        // still need them. Reuse the queue's allocation after retiring that batch.
        let mut stale = std::mem::take(&mut self.stale);
        for (space, pn) in stale.drain(..) {
            self.remove_at(space, pn);
        }
        self.stale = stale;
        for (space, pn) in self.acknowledged.drain(..) {
            if let Some(packet) = self.spaces[space].get_mut(&pn)
                && !packet.stale
            {
                packet.stale = true;
                self.stale.push((space, pn));
            }
        }
        while let Some(&(sent_round, space, pn)) = self.expiry.first()
            && round > sent_round.saturating_add(ROUND_COUNT_WINDOW)
        {
            self.expiry.pop_first();
            if let Some(packet) = self.spaces[space].get_mut(&pn)
                && !packet.stale
            {
                packet.stale = true;
                self.stale.push((space, pn));
            }
        }
    }

    #[cfg(test)]
    pub(super) fn scanned() -> Self {
        Self {
            scan: Some(Default::default()),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn last(&self, space: SpaceId) -> Option<BbrPacket> {
        let space = space as usize;
        if let Some(scan) = &self.scan {
            return scan[space].back().copied();
        }
        self.spaces[space]
            .last_key_value()
            .map(|(_, packet)| *packet)
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> Vec<BbrPacket> {
        if let Some(scan) = &self.scan {
            return scan.iter().flatten().copied().collect();
        }
        self.spaces
            .iter()
            .flat_map(|packets| packets.values().copied())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Instant;

    fn packet(space: SpaceId, pn: u64, round: u64) -> BbrPacket {
        let now = Instant::now();
        BbrPacket {
            delivered: 0,
            delivered_time: now,
            first_send_time: now,
            send_time: now,
            is_app_limited: false,
            tx_in_flight: 1200,
            packet_number: pn,
            space,
            size: 1200,
            lost: 0,
            acknowledged: false,
            stale: false,
            round_count: round,
        }
    }

    #[test]
    fn duplicate_completions_keep_one_key_and_one_ack_batch_of_grace() {
        let mut packets = Packets::default();
        packets.insert(packet(SpaceId::Data, 7, 0));
        assert!(packets.acknowledge(SpaceId::Data, 7).is_some());
        assert!(packets.acknowledge(SpaceId::Data, 7).is_some());
        assert_eq!(packets.acknowledged.len(), 1);
        packets.cleanup(0);
        assert_eq!(packets.snapshot().len(), 1);
        // Loss after the ACK can still read that packet's snapshot.
        assert!(packets.remove(SpaceId::Data, 7).is_some());
        packets.cleanup(0);
        assert!(packets.acknowledge(SpaceId::Data, 7).is_none());
        assert!(packets.expiry.is_empty());
        assert!(packets.stale.is_empty());
        assert!(packets.acknowledged.is_empty());
    }

    #[test]
    fn expired_spaces_never_reuse_another_spaces_metadata() {
        let mut packets = Packets::default();
        packets.insert(packet(SpaceId::Initial, 0, 0));
        packets.insert(packet(SpaceId::Handshake, 0, 0));
        let data = packet(SpaceId::Data, 0, ROUND_COUNT_WINDOW + 1);
        packets.cleanup(ROUND_COUNT_WINDOW);
        assert_eq!(packets.snapshot().len(), 2);
        packets.insert(data);
        packets.cleanup(ROUND_COUNT_WINDOW + 1);
        assert_eq!(
            packets.snapshot().len(),
            3,
            "expiry keeps the same one-batch grace"
        );
        packets.cleanup(ROUND_COUNT_WINDOW + 1);
        assert!(packets.acknowledge(SpaceId::Initial, 0).is_none());
        assert!(packets.remove(SpaceId::Handshake, 0).is_none());
        let remaining = packets.acknowledge(SpaceId::Data, 0).unwrap();
        assert_eq!(remaining.send_time, data.send_time);
        assert_eq!(remaining.space, SpaceId::Data);
        assert_eq!(packets.expiry.len(), 1);
    }

    #[test]
    fn sparse_steady_flights_bound_every_index() {
        let mut packets = Packets::default();
        for pn in 0..32 {
            packets.insert(packet(SpaceId::Data, pn * 1_000_000, 0));
        }
        let mut live: Vec<_> = (0..32).map(|pn| pn * 1_000_000).collect();
        for turn in 0..4096u64 {
            let slot = (31 - turn % 32) as usize;
            let old = live[slot];
            let next = 1_000_000_000_000 + turn;
            assert!(packets.acknowledge(SpaceId::Data, old).is_some());
            packets.cleanup(turn / 32);
            packets.insert(packet(SpaceId::Data, next, turn / 32));
            live[slot] = next;
            assert!(packets.spaces.iter().map(BTreeMap::len).sum::<usize>() <= 33);
            assert!(packets.expiry.len() <= 33);
            assert!(packets.stale.len() <= 1);
            assert!(packets.acknowledged.is_empty());
        }
        // A lost or completed key leaves no expiry record that a later round scans.
        for pn in live {
            packets.remove(SpaceId::Data, pn);
        }
        packets.cleanup(4096);
        assert!(packets.snapshot().is_empty());
        assert!(packets.expiry.is_empty());
        assert!(packets.stale.is_empty());
    }
}
