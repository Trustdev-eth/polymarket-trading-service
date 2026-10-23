use crate::types::{CancelReason, Command, IntentKind, OrderIntent};

const SLAB: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Empty,
    PendingAck,
    Live,
    PendingCancel,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    state: SlotState,
    intent: Option<OrderIntent>,
    cancel_queued: Option<CancelReason>,
    since_ms: u64,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            state: SlotState::Empty,
            intent: None,
            cancel_queued: None,
            since_ms: 0,
        }
    }
}

/// Fixed-size order table with ack-gated cancels.
#[derive(Clone, Debug)]
pub struct Oms {
    slots: [Slot; SLAB],
    next_seq: u64,
}

impl Default for Oms {
    fn default() -> Self {
        Self {
            slots: [Slot::default(); SLAB],
            next_seq: 1,
        }
    }
}

impl Oms {
    /// Clears slots; sequence numbers keep increasing across markets.
    pub fn reset(&mut self) {
        self.slots = [Slot::default(); SLAB];
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn find(&self, seq: u64) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.state != SlotState::Empty && s.intent.is_some_and(|i| i.client_seq == seq))
    }

    pub fn submit(&mut self, intent: OrderIntent, now_ms: u64) -> bool {
        let Some(idx) = self.slots.iter().position(|s| s.state == SlotState::Empty) else {
            return false;
        };
        self.slots[idx] = Slot {
            state: SlotState::PendingAck,
            intent: Some(intent),
            cancel_queued: None,
            since_ms: now_ms,
        };
        true
    }

    /// Live → cancel now. PendingAck → hold until the ack.
    pub fn request_cancel(&mut self, seq: u64, reason: CancelReason, now_ms: u64, out: &mut Vec<Command>) {
        let Some(idx) = self.find(seq) else {
            return;
        };
        let slot = &mut self.slots[idx];
        match slot.state {
            SlotState::Live => {
                slot.state = SlotState::PendingCancel;
                slot.since_ms = now_ms;
                out.push(Command::Cancel { client_seq: seq, reason });
            }
            SlotState::PendingAck => slot.cancel_queued = Some(reason),
            SlotState::PendingCancel | SlotState::Empty => {}
        }
    }

    pub fn cancel_kind(&mut self, kind: IntentKind, reason: CancelReason, now_ms: u64, out: &mut Vec<Command>) {
        for seq in self.seqs(|i| i.kind == kind) {
            self.request_cancel(seq, reason, now_ms, out);
        }
    }

    pub fn cancel_all(&mut self, reason: CancelReason, now_ms: u64, out: &mut Vec<Command>) {
        for seq in self.seqs(|_| true) {
            self.request_cancel(seq, reason, now_ms, out);
        }
    }

    fn seqs(&self, pred: impl Fn(&OrderIntent) -> bool) -> Vec<u64> {
        self.slots
            .iter()
            .filter(|s| s.state != SlotState::Empty)
            .filter_map(|s| s.intent.filter(|i| pred(i)).map(|i| i.client_seq))
            .collect()
    }

    /// Create ack, or cancel ack for a `PendingCancel` slot. FAK/FOK finish at ack.
    pub fn on_ack(&mut self, seq: u64, now_ms: u64, out: &mut Vec<Command>) {
        let Some(idx) = self.find(seq) else {
            return;
        };
        let slot = &mut self.slots[idx];
        match slot.state {
            SlotState::PendingCancel => *slot = Slot::default(),
            SlotState::PendingAck if slot.intent.is_some_and(|i| i.tif.is_immediate()) => {
                *slot = Slot::default()
            }
            SlotState::PendingAck => {
                slot.state = SlotState::Live;
                slot.since_ms = now_ms;
                if let Some(reason) = slot.cancel_queued.take() {
                    slot.state = SlotState::PendingCancel;
                    out.push(Command::Cancel { client_seq: seq, reason });
                }
            }
            SlotState::Live | SlotState::Empty => {}
        }
    }

    pub fn on_reject(&mut self, seq: u64) {
        if let Some(idx) = self.find(seq) {
            self.slots[idx] = Slot::default();
        }
    }

    pub fn on_fill(&mut self, seq: u64, size: f64) {
        let Some(idx) = self.find(seq) else {
            return;
        };
        let slot = &mut self.slots[idx];
        if let Some(intent) = slot.intent.as_mut() {
            intent.size -= size;
            if intent.size <= 1e-9 {
                *slot = Slot::default();
            }
        }
    }

    /// Drop slots whose ack never arrived (gateway restart, lost frame).
    pub fn expire(&mut self, now_ms: u64, timeout_ms: u64) {
        for s in &mut self.slots {
            if matches!(s.state, SlotState::PendingAck | SlotState::PendingCancel)
                && now_ms.saturating_sub(s.since_ms) > timeout_ms
            {
                *s = Slot::default();
            }
        }
    }

    pub fn intent(&self, seq: u64) -> Option<OrderIntent> {
        self.find(seq).and_then(|i| self.slots[i].intent)
    }

    pub fn state(&self, seq: u64) -> SlotState {
        self.find(seq).map(|i| self.slots[i].state).unwrap_or(SlotState::Empty)
    }

    pub fn has_kind(&self, kind: IntentKind) -> bool {
        self.slots
            .iter()
            .any(|s| s.state != SlotState::Empty && s.intent.is_some_and(|i| i.kind == kind))
    }

    pub fn in_flight(&self, kind: IntentKind) -> bool {
        self.slots.iter().any(|s| {
            matches!(s.state, SlotState::PendingAck | SlotState::PendingCancel)
                && s.intent.is_some_and(|i| i.kind == kind)
        })
    }

    pub fn live(&self, kind: IntentKind) -> Option<OrderIntent> {
        self.slots
            .iter()
            .find(|s| s.state == SlotState::Live && s.intent.is_some_and(|i| i.kind == kind))
            .and_then(|s| s.intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Side, Tif, TokenRef};

    fn intent(seq: u64, kind: IntentKind, tif: Tif) -> OrderIntent {
        OrderIntent {
            token: TokenRef::Yes,
            side: Side::Buy,
            px_ticks: 50,
            size: 5.0,
            tif,
            post_only: false,
            created_ms: 1,
            client_seq: seq,
            kind,
        }
    }

    #[test]
    fn ack_gates_cancel() {
        let mut oms = Oms::default();
        let mut out = Vec::new();
        oms.submit(intent(1, IntentKind::Hedge, Tif::Gtc), 0);
        oms.request_cancel(1, CancelReason::Replace, 0, &mut out);
        assert!(out.is_empty());
        assert!(oms.in_flight(IntentKind::Hedge));
        oms.on_ack(1, 10, &mut out);
        assert_eq!(out, vec![Command::Cancel { client_seq: 1, reason: CancelReason::Replace }]);
        assert_eq!(oms.state(1), SlotState::PendingCancel);
        out.clear();
        oms.on_ack(1, 11, &mut out);
        assert_eq!(oms.state(1), SlotState::Empty);
        assert!(out.is_empty());
    }

    #[test]
    fn fak_ack_frees_slot_without_delete() {
        let mut oms = Oms::default();
        let mut out = Vec::new();
        oms.submit(intent(1, IntentKind::Taker, Tif::Fak), 0);
        oms.request_cancel(1, CancelReason::Jump, 0, &mut out);
        oms.on_ack(1, 10, &mut out);
        assert!(out.is_empty(), "FAK ack must not DELETE, got {out:?}");
        assert!(!oms.has_kind(IntentKind::Taker));
    }

    #[test]
    fn partial_fills_then_free() {
        let mut oms = Oms::default();
        let mut out = Vec::new();
        oms.submit(intent(1, IntentKind::Hedge, Tif::Gtc), 0);
        oms.on_ack(1, 1, &mut out);
        oms.on_fill(1, 2.0);
        assert!((oms.live(IntentKind::Hedge).unwrap().size - 3.0).abs() < 1e-9);
        oms.on_fill(1, 3.0);
        assert_eq!(oms.state(1), SlotState::Empty);
    }

    #[test]
    fn expire_drops_unacked() {
        let mut oms = Oms::default();
        oms.submit(intent(1, IntentKind::Taker, Tif::Fak), 0);
        oms.expire(5_000, 10_000);
        assert!(oms.has_kind(IntentKind::Taker));
        oms.expire(10_001, 10_000);
        assert!(!oms.has_kind(IntentKind::Taker));
    }

    #[test]
    fn cancel_all_queues_pending_and_cancels_live() {
        let mut oms = Oms::default();
        let mut out = Vec::new();
        oms.submit(intent(1, IntentKind::Hedge, Tif::Gtc), 0);
        oms.submit(intent(2, IntentKind::Hedge, Tif::Gtc), 0);
        oms.on_ack(2, 1, &mut out);
        oms.cancel_all(CancelReason::Halt, 2, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(oms.state(1), SlotState::PendingAck);
        assert_eq!(oms.state(2), SlotState::PendingCancel);
    }
}
