//! Shared queue-ticket infrastructure.
//!
//! `QueueTicket` is the RAII handle returned by `Scheduler::admit`.  It owns
//! a drop handler that releases the admission slot, decrements
//! `llm_active_flows`, and notifies completion-bias waiters, guaranteeing
//! slot release on all exit paths: success, error, panic (Drop runs on
//! unwind), and client disconnect (future handler drops).
//!
//! DRR (and the gateway stream) use [`make_ticket`] to build tickets with
//! drop handlers and [`QueueTicket::disarm`] to neutralize a ticket whose
//! oneshot delivery failed.

use crate::flow::FlowId;

/// RAII ticket returned by `Scheduler::admit`.
///
/// When dropped, it:
/// 1. Releases the admission slot.
/// 2. Decrements `llm_active_flows`.
///
/// This guarantees slot release on **all** exit paths: success, error,
/// panic (Drop runs on unwind), and client disconnect (future handler drops).
// @lat: [[scheduler#Queue Ticket]]
pub struct QueueTicket {
    /// The flow ID associated with this ticket.
    pub flow_id: FlowId,
    /// Work unit (estimated max_tokens) for this request.
    pub work_unit: f64,
    /// Combined drop handler: releases the permit and reports completion.
    /// Wrapped in Option so it can be taken() in Drop (FnOnce can only be
    /// called once, and Drop takes &mut self).
    drop_handler: Option<Box<dyn Send + FnOnce()>>,
}

impl QueueTicket {
    /// Disarm this ticket so its drop handler does NOT run on Drop.
    ///
    /// Used by the admission loop when the oneshot send fails: the receiver
    /// is gone (timeout or abort), so we must prevent the drop handler from
    /// decrementing `active_flows`, crediting `service_done`, and releasing the
    /// permit. The caller is responsible for releasing the permit exactly once.
    pub fn disarm(&mut self) {
        self.drop_handler.take();
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        // Take the handler out of the Option (FnOnce can only be called once).
        if let Some(handler) = self.drop_handler.take() {
            handler();
        }
    }
}

/// Construct a `QueueTicket` from a flow ID, work unit, and a drop handler closure.
///
/// The `drop_handler` closure is called on drop to release the permit
/// and report completion.
pub fn make_ticket(
    flow_id: FlowId,
    work_unit: f64,
    drop_handler: impl FnOnce() + Send + 'static,
) -> QueueTicket {
    QueueTicket {
        flow_id,
        work_unit,
        drop_handler: Some(Box::new(drop_handler)),
    }
}
