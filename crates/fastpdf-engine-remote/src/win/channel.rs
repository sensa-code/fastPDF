//! The slot channel (ADR 0008 §1.4): tile renders without pipe round trips.
//!
//! A tile render used to be a `Render` command on the command pipe, read by
//! the host's main thread and handed to a render thread, and a `Done` reply
//! read by FastPDF's reply reader and handed to the waiting worker: four
//! thread wake-ups, two pipe writes and two pipe reads per tile. Each slot
//! now has a control block in a second shared section:
//!
//! 1. The parent writes the encoded `Render` command into the slot's block
//!    and marks it `REQUESTED` (under its table lock, so requests are posted
//!    in the order they were registered), then releases the request
//!    semaphore.
//! 2. A host render thread waiting on the semaphore takes the requested
//!    block with the lowest order (`REQUESTED` → `TAKEN`), renders into the
//!    slot, writes the encoded `Done` reply into the block, marks it `DONE`
//!    and sets the slot's completion event.
//! 3. The requesting worker, waiting on that event (and on the "host
//!    ended" event), checks and decodes the reply and copies the pixels.
//!
//! The reply reader is not involved, and a host render thread that finds
//! more requests waiting takes the next one without sleeping. Cancellation
//! still uses the command pipe; the block's cancel flag covers requests the
//! host has not taken yet.
//!
//! Neither side trusts the other's writes: the host decodes the request
//! with the protocol decoder, and the parent checks state, id and length
//! before decoding the reply.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::protocol::{MAX_SLOTS, SLOT_CONTROL_BYTES};

use super::section::View;

/// Bytes of one control block.
const BLOCK: usize = SLOT_CONTROL_BYTES as usize;
const STATE_AT: usize = 0;
const CANCEL_AT: usize = 4;
const ID_AT: usize = 8;
const ORDER_AT: usize = 16;
const REQUEST_LEN_AT: usize = 24;
const REPLY_LEN_AT: usize = 28;
const REQUEST_AT: usize = 64;
const REPLY_AT: usize = 512;
/// Longest encoded `Render` command a block holds.
pub(crate) const MAX_REQUEST: usize = REPLY_AT - REQUEST_AT;
/// Longest encoded `Done` reply a block holds.
pub(crate) const MAX_REPLY: usize = BLOCK - REPLY_AT;

/// No request, or the parent has collected the reply.
const FREE: u32 = 0;
/// Written by the parent, not taken yet.
const REQUESTED: u32 = 1;
/// A host render thread is working on it.
const TAKEN: u32 = 2;
/// The reply is in the block.
const DONE: u32 = 3;

/// Why a reply cannot be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyError {
    NotDone,
    WrongId,
    BadLength,
}

/// The control blocks of every slot, mapped read-write.
#[derive(Debug)]
pub(crate) struct Controls {
    view: View,
    count: u32,
}

impl Controls {
    /// `view` must map `count` blocks, writable.
    pub(crate) fn new(view: View, count: u32) -> Option<Self> {
        (count <= MAX_SLOTS && view.len() >= count as usize * BLOCK).then_some(Self { view, count })
    }

    pub(crate) fn count(&self) -> u32 {
        self.count
    }

    fn at(&self, slot: u32, offset: usize) -> Option<usize> {
        (slot < self.count).then(|| slot as usize * BLOCK + offset)
    }

    fn word(&self, slot: u32, offset: usize) -> Option<&AtomicU32> {
        self.view.atomic_u32(self.at(slot, offset)?)
    }

    fn word64(&self, slot: u32, offset: usize) -> Option<&AtomicU64> {
        self.view.atomic_u64(self.at(slot, offset)?)
    }

    // --- parent ----------------------------------------------------------

    /// Posts request `id` (an encoded `Render` command) in `slot`. False if
    /// the slot or the request does not fit.
    pub(crate) fn post(&self, slot: u32, id: u64, order: u64, request: &[u8]) -> bool {
        let (Some(state), Some(cancel), Some(id_word), Some(order_word)) = (
            self.word(slot, STATE_AT),
            self.word(slot, CANCEL_AT),
            self.word64(slot, ID_AT),
            self.word64(slot, ORDER_AT),
        ) else {
            return false;
        };
        let (Some(request_len), Some(reply_len), Some(bytes_at)) = (
            self.word(slot, REQUEST_LEN_AT),
            self.word(slot, REPLY_LEN_AT),
            self.at(slot, REQUEST_AT),
        ) else {
            return false;
        };
        if request.len() > MAX_REQUEST || !self.view.write_shared(bytes_at, request) {
            return false;
        }
        id_word.store(id, Ordering::Relaxed);
        order_word.store(order, Ordering::Relaxed);
        cancel.store(0, Ordering::Relaxed);
        request_len.store(request.len() as u32, Ordering::Relaxed);
        reply_len.store(0, Ordering::Relaxed);
        // Publishes everything above to the host's Acquire.
        state.store(REQUESTED, Ordering::Release);
        true
    }

    /// Asks the host not to start (or to stop) request `id` in `slot`.
    pub(crate) fn cancel(&self, slot: u32) {
        if let Some(cancel) = self.word(slot, CANCEL_AT) {
            cancel.store(1, Ordering::Release);
        }
    }

    /// True when `slot` holds the reply to request `id`.
    pub(crate) fn is_done(&self, slot: u32, id: u64) -> bool {
        self.word(slot, STATE_AT)
            .is_some_and(|s| s.load(Ordering::Acquire) == DONE)
            && self
                .word64(slot, ID_AT)
                .is_some_and(|w| w.load(Ordering::Relaxed) == id)
    }

    /// The encoded reply to request `id`; the slot is free again.
    pub(crate) fn reply(&self, slot: u32, id: u64) -> Result<Vec<u8>, ReplyError> {
        let state = self.word(slot, STATE_AT).ok_or(ReplyError::NotDone)?;
        if state.load(Ordering::Acquire) != DONE {
            return Err(ReplyError::NotDone);
        }
        if self
            .word64(slot, ID_AT)
            .is_none_or(|w| w.load(Ordering::Relaxed) != id)
        {
            return Err(ReplyError::WrongId);
        }
        let len = self
            .word(slot, REPLY_LEN_AT)
            .map_or(0, |w| w.load(Ordering::Relaxed) as usize);
        if len == 0 || len > MAX_REPLY {
            return Err(ReplyError::BadLength);
        }
        let mut bytes = vec![0; len];
        let at = self.at(slot, REPLY_AT).ok_or(ReplyError::BadLength)?;
        if !self.view.read_into(at, &mut bytes) {
            return Err(ReplyError::BadLength);
        }
        state.store(FREE, Ordering::Relaxed);
        Ok(bytes)
    }

    // --- host --------------------------------------------------------------

    /// Takes the requested block with the lowest order: its slot, request id
    /// and encoded command. `None` if no block is requested.
    pub(crate) fn take(&self) -> Option<(u32, u64, Vec<u8>)> {
        loop {
            let mut best: Option<(u64, u32)> = None;
            for slot in 0..self.count {
                if self
                    .word(slot, STATE_AT)
                    .is_some_and(|s| s.load(Ordering::Acquire) == REQUESTED)
                {
                    let order = self
                        .word64(slot, ORDER_AT)
                        .map_or(u64::MAX, |w| w.load(Ordering::Relaxed));
                    if best.is_none_or(|(o, _)| order < o) {
                        best = Some((order, slot));
                    }
                }
            }
            let (_, slot) = best?;
            let state = self.word(slot, STATE_AT)?;
            if state
                .compare_exchange(REQUESTED, TAKEN, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                // Another render thread took it first.
                continue;
            }
            let id = self.word64(slot, ID_AT)?.load(Ordering::Relaxed);
            let len = (self.word(slot, REQUEST_LEN_AT)?.load(Ordering::Relaxed) as usize)
                .min(MAX_REQUEST);
            let mut bytes = vec![0; len];
            if !self.view.read_into(self.at(slot, REQUEST_AT)?, &mut bytes) {
                bytes.clear();
            }
            return Some((slot, id, bytes));
        }
    }

    pub(crate) fn cancelled(&self, slot: u32) -> bool {
        self.word(slot, CANCEL_AT)
            .is_some_and(|c| c.load(Ordering::Acquire) != 0)
    }

    /// Stores the encoded reply of the request in `slot` and marks it done;
    /// false if the reply does not fit (nothing is stored then).
    pub(crate) fn finish(&self, slot: u32, reply: &[u8]) -> bool {
        let (Some(state), Some(reply_len), Some(at)) = (
            self.word(slot, STATE_AT),
            self.word(slot, REPLY_LEN_AT),
            self.at(slot, REPLY_AT),
        ) else {
            return false;
        };
        if reply.is_empty() || reply.len() > MAX_REPLY || !self.view.write_shared(at, reply) {
            return false;
        }
        reply_len.store(reply.len() as u32, Ordering::Relaxed);
        // Publishes the reply to the parent's Acquire.
        state.store(DONE, Ordering::Release);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::section::{Access, Section};

    fn controls(count: u32) -> (Section, Controls, Controls) {
        let section = Section::create(count as usize * BLOCK).unwrap();
        let parent = Controls::new(section.map(Access::ReadWrite).unwrap(), count).unwrap();
        let host = Controls::new(section.map(Access::ReadWrite).unwrap(), count).unwrap();
        (section, parent, host)
    }

    #[test]
    fn requests_travel_in_order_and_replies_come_back() {
        let (_section, parent, host) = controls(3);
        assert_eq!(host.take(), None);
        assert!(parent.post(2, 41, 7, b"second"));
        assert!(parent.post(0, 40, 6, b"first"));
        assert!(!parent.is_done(0, 40));
        // The lowest order first, whatever the slot.
        assert_eq!(host.take(), Some((0, 40, b"first".to_vec())));
        assert_eq!(host.take(), Some((2, 41, b"second".to_vec())));
        assert_eq!(host.take(), None, "taken requests are not taken again");
        assert_eq!(parent.reply(0, 40), Err(ReplyError::NotDone));
        assert!(host.finish(0, b"reply"));
        assert!(parent.is_done(0, 40));
        assert_eq!(parent.reply(0, 41), Err(ReplyError::WrongId));
        assert_eq!(parent.reply(0, 40), Ok(b"reply".to_vec()));
        // Collected: the slot is free and can carry the next request.
        assert_eq!(parent.reply(0, 40), Err(ReplyError::NotDone));
        assert!(parent.post(0, 42, 8, b"third"));
        assert_eq!(host.take(), Some((0, 42, b"third".to_vec())));
    }

    #[test]
    fn cancel_flags_and_limits() {
        let (_section, parent, host) = controls(2);
        assert!(parent.post(1, 5, 1, b"r"));
        assert!(!host.cancelled(1));
        parent.cancel(1);
        assert!(host.cancelled(1));
        // A new request clears the flag.
        assert!(parent.post(1, 6, 2, b"r"));
        assert!(!host.cancelled(1));
        assert!(!parent.post(2, 7, 3, b"r"), "no such slot");
        assert!(!parent.post(0, 7, 3, &vec![0; MAX_REQUEST + 1]));
        assert!(!host.finish(0, &[]));
        assert!(!host.finish(0, &vec![0; MAX_REPLY + 1]));
        assert_eq!(parent.reply(9, 1), Err(ReplyError::NotDone));
    }
}
