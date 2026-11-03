//! Simulator-independent startup, commands, metadata, value recording and output.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    HierarchyElement, MAX_VALUE_CHANGES_SIZE, PROTOCOL_VERSION, SignalChange, SignalValue, SimId,
    SimulationStatus, SimulationTimestamp, SimulatorToSurferMessage, TimeStep,
    append_value_changes_time_step, encode_simulator_to_surfer_message,
    encode_value_changes_buffer,
};

#[cfg(all(feature = "plugin-tcp", not(target_arch = "wasm32")))]
pub mod tcp;

/// Callback handles indexed by signal ID, independent of the simulator API.
/// Backends wrap native handles before insertion and unregister them after removal.
pub struct CallbackStore<Handle> {
    callbacks: Mutex<HashMap<SimId, Handle>>,
}

impl<Handle> Default for CallbackStore<Handle> {
    fn default() -> Self {
        Self {
            callbacks: Mutex::new(HashMap::new()),
        }
    }
}

impl<Handle> CallbackStore<Handle> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains_callback(&self, id: SimId) -> bool {
        self.callbacks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&id)
    }

    pub fn insert_callback(&self, id: SimId, handle: Handle) {
        // Release the map lock before dropping a replaced handle: backend cleanup
        // may call into the simulator or invoke another callback.
        let previous = self
            .callbacks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id, handle);
        drop(previous);
    }

    pub fn remove_callback(&self, id: SimId) -> Option<Handle> {
        self.callbacks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id)
    }
}

/// Requests queued by the connection task for execution on the simulator thread.
pub enum SimulatorCommand {
    Message {
        message: crate::SurferToSimulatorMessage,
        response: tokio::sync::oneshot::Sender<SimulatorToSurferMessage>,
    },
    /// Sent after acknowledging shutdown so the simulator can terminate safely.
    Finish,
}

/// Startup options common to the VPI and VHPI plugins. Unknown arguments are ignored.
#[derive(Debug, PartialEq, Eq)]
pub struct StartupOptions {
    pub address: String,
    pub debug: bool,
    pub no_info: bool,
}

impl StartupOptions {
    /// Address and port are aliases; the last occurrence wins.
    pub fn parse(arguments: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut address = String::new();
        let mut debug = false;
        let mut no_info = false;
        for argument in arguments {
            match argument.as_ref() {
                "+debug" => debug = true,
                "+no-info" => no_info = true,
                argument => {
                    if let Some(value) = argument
                        .strip_prefix("+address=")
                        .or_else(|| argument.strip_prefix("+port="))
                    {
                        address = value.to_owned();
                    }
                }
            }
        }
        Self {
            address: crate::simulator_address(&address),
            debug,
            no_info,
        }
    }
}

/// Fixed-width, most-significant-bit-first binary digits, without a VCD prefix.
pub fn uint_to_bit_string(value: impl Into<u64>, bits: usize) -> String {
    let value = value.into();
    (0..bits)
        .rev()
        .map(|i| if (value >> i) & 1 == 1 { '1' } else { '0' })
        .collect()
}

/// Signed counterpart, preserving the requested two's-complement bits.
pub fn int_to_bit_string(value: impl Into<i64>, bits: usize) -> String {
    uint_to_bit_string(value.into() as u64, bits)
}

/// Immutable simulator metadata alongside a backend-owned signal handle map.
/// Handle access and hierarchy traversal remain the backend's responsibility.
pub struct SimulatorData<Handles> {
    pub hierarchy: Vec<HierarchyElement>,
    pub id_to_handle: Arc<Handles>,
    pub simulator_name: String,
    pub simulator_version: String,
    pub time_precision: i32,
}

impl<Handles> SimulatorData<Handles> {
    pub fn simulator_info(&self) -> SimulatorToSurferMessage {
        SimulatorToSurferMessage::SimulatorInfo {
            protocol_version: PROTOCOL_VERSION,
            name: self.simulator_name.clone(),
            version: self.simulator_version.clone(),
        }
    }

    pub fn hierarchy_message(&self) -> SimulatorToSurferMessage {
        SimulatorToSurferMessage::Hierarchy {
            hierarchy: self.hierarchy.clone(),
            exponent: self.time_precision,
        }
    }
}

/// Recorded changes in callback order, grouped by consecutive equal timestamps.
/// Multiple changes to the same signal in a timestep are preserved.
#[derive(Default)]
pub struct ValueChangeBuffer {
    time_steps: Vec<TimeStep>,
}

impl ValueChangeBuffer {
    /// Preserve a pause/completion timestamp even when no signals changed.
    pub fn ensure_time_step(&mut self, timestamp: SimulationTimestamp) {
        if self
            .time_steps
            .last()
            .is_none_or(|step| step.timestamp != timestamp.ticks())
        {
            self.time_steps.push(TimeStep {
                timestamp: timestamp.ticks(),
                changes: Vec::new(),
            });
        }
    }

    /// Register an already converted value; simulator API calls stay in the plugin.
    pub fn record(&mut self, timestamp: SimulationTimestamp, id: SimId, value: SignalValue) {
        self.ensure_time_step(timestamp);
        self.time_steps
            .last_mut()
            .expect("timestamp was just ensured")
            .changes
            .push(SignalChange { id, value });
    }

    /// Remove pending values for untracked signals and discard empty timesteps.
    pub fn remove_signals(&mut self, ids: &HashSet<SimId>) {
        for step in &mut self.time_steps {
            step.changes.retain(|change| !ids.contains(&change.id));
        }
        self.time_steps.retain(|step| !step.changes.is_empty());
    }

    /// Drain before encoding or sending, so callers can release their lock first.
    pub fn take(&mut self) -> Vec<TimeStep> {
        std::mem::take(&mut self.time_steps)
    }
}

/// Queue complete logical messages on the simulator thread. The connection
/// task must send each message with `transport::write_message`.
pub struct ClientConnection {
    outgoing: UnboundedSender<Vec<u8>>,
}

impl ClientConnection {
    pub fn new(outgoing: UnboundedSender<Vec<u8>>) -> Self {
        Self { outgoing }
    }

    pub fn send(&self, message: SimulatorToSurferMessage) {
        if let Ok(encoded) = encode_simulator_to_surfer_message(&message) {
            self.send_encoded(encoded);
        }
    }

    pub fn send_encoded(&self, encoded: Vec<u8>) {
        let _ = self.outgoing.send(encoded);
    }

    /// Emit bounded target-size batches. A single large timestep is allowed:
    /// transport chunking handles it without splitting its timestamp semantics.
    /// Always emit terminal status, including when there are no changes.
    pub fn send_value_changes(&self, time_steps: Vec<TimeStep>, status: SimulationStatus) {
        encode_value_change_batches(time_steps, status, |encoded| self.send_encoded(encoded));
    }
}

pub fn encode_value_change_batches(
    time_steps: impl IntoIterator<Item = TimeStep>,
    status: SimulationStatus,
    mut emit: impl FnMut(Vec<u8>),
) {
    let mut buffer = Vec::new();
    for time_step in time_steps {
        let mut next = Vec::new();
        append_value_changes_time_step(&mut next, &time_step);
        if !buffer.is_empty() && buffer.len() + next.len() > MAX_VALUE_CHANGES_SIZE {
            emit(encode_value_changes_buffer(
                &buffer,
                SimulationStatus::Running,
            ));
            buffer.clear();
        }
        buffer.extend_from_slice(&next);
    }
    if !buffer.is_empty() || status != SimulationStatus::Running {
        emit(encode_value_changes_buffer(&buffer, status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SignalChange, SignalValue, decode_simulator_to_surfer_message};

    #[test]
    fn callback_store_tracks_replaces_and_transfers_handles_without_a_default_bound() {
        struct Handle(u32);
        let callbacks = CallbackStore::new();
        assert!(!callbacks.contains_callback(7));
        assert!(callbacks.remove_callback(7).is_none());
        callbacks.insert_callback(7, Handle(10));
        callbacks.insert_callback(8, Handle(20));
        callbacks.insert_callback(7, Handle(30));
        assert!(callbacks.contains_callback(7));
        assert_eq!(callbacks.remove_callback(7).unwrap().0, 30);
        assert!(!callbacks.contains_callback(7));
        assert_eq!(callbacks.remove_callback(8).unwrap().0, 20);
    }

    #[test]
    fn callback_store_releases_the_lock_before_handle_cleanup() {
        struct Handle(
            std::sync::Weak<CallbackStore<Handle>>,
            Arc<std::sync::atomic::AtomicUsize>,
        );
        impl Drop for Handle {
            fn drop(&mut self) {
                if let Some(store) = self.0.upgrade() {
                    assert!(store.callbacks.try_lock().is_ok());
                }
                self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let callbacks = Arc::new(CallbackStore::new());
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handle = || Handle(Arc::downgrade(&callbacks), Arc::clone(&dropped));
        callbacks.insert_callback(7, handle());
        callbacks.insert_callback(7, handle());
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
        let removed = callbacks.remove_callback(7).unwrap();
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(removed);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn startup_options_share_defaults_flags_and_address_precedence() {
        let defaults = StartupOptions::parse(["design.vvp", "+unrelated"]);
        assert_eq!(defaults.address, crate::simulator_address(""));
        assert!(!defaults.debug);
        assert!(!defaults.no_info);
        for (arguments, expected) in [
            (vec!["+port=7777"], "localhost:7777"),
            (vec!["+address=simulator.example"], "simulator.example:6967"),
            (vec!["+port=7777", "+address=[::1]:8888"], "[::1]:8888"),
            (
                vec!["+address=simulator.example", "+port=7777"],
                "localhost:7777",
            ),
        ] {
            let options =
                StartupOptions::parse(arguments.into_iter().chain(["+debug", "+no-info"]));
            assert_eq!(options.address, expected);
            assert!(options.debug);
            assert!(options.no_info);
        }
    }

    #[test]
    fn recording_preserves_callback_order_and_repeated_signal_changes() {
        let mut buffer = ValueChangeBuffer::default();
        for (time, id, value) in [(3, 1, b'0'), (3, 1, b'1'), (3, 2, b'x'), (4, 1, b'0')] {
            buffer.record(
                SimulationTimestamp::from_ticks(time),
                id,
                SignalValue::VCDValue(vec![value]),
            );
        }
        let steps = buffer.take();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].timestamp, 3);
        assert_eq!(steps[1].timestamp, 4);
        let changes: Vec<_> = steps[0]
            .changes
            .iter()
            .map(|change| {
                let SignalValue::VCDValue(value) = &change.value;
                (change.id, value[0])
            })
            .collect();
        assert_eq!(changes, [(1, b'0'), (1, b'1'), (2, b'x')]);
        assert!(buffer.take().is_empty());
    }

    #[test]
    fn untracking_purges_pending_values_and_preserves_other_signals() {
        let mut buffer = ValueChangeBuffer::default();
        for (time, id) in [(3, 1), (3, 2), (4, 1)] {
            buffer.record(
                SimulationTimestamp::from_ticks(time),
                id,
                SignalValue::VCDValue(b"r1.0".to_vec()),
            );
        }
        buffer.remove_signals(&HashSet::from([1]));
        let steps = buffer.take();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].timestamp, 3);
        assert_eq!(steps[0].changes.len(), 1);
        assert_eq!(steps[0].changes[0].id, 2);

        // Even with no tracked values, pause/completion must convey the time.
        buffer.ensure_time_step(SimulationTimestamp::from_ticks(8));
        buffer.ensure_time_step(SimulationTimestamp::from_ticks(8));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        ClientConnection::new(tx).send_value_changes(buffer.take(), SimulationStatus::Paused);
        let SimulatorToSurferMessage::ValueChanges { time_steps, status } =
            decode_simulator_to_surfer_message(&rx.try_recv().unwrap()).unwrap()
        else {
            panic!("wrong message type")
        };
        assert_eq!(status, SimulationStatus::Paused);
        assert_eq!(time_steps.len(), 1);
        assert_eq!(time_steps[0].timestamp, 8);
        assert!(time_steps[0].changes.is_empty());
    }

    #[test]
    fn batching_preserves_timesteps_and_only_finishes_last_batch() {
        let steps = (0..3).map(|timestamp| TimeStep {
            timestamp,
            changes: vec![SignalChange {
                id: 1,
                value: SignalValue::VCDValue(vec![b'1'; MAX_VALUE_CHANGES_SIZE]),
            }],
        });
        let mut messages = Vec::new();
        encode_value_change_batches(steps, SimulationStatus::Done, |encoded| {
            messages.push(encoded)
        });
        assert_eq!(messages.len(), 3);
        for (index, encoded) in messages.iter().enumerate() {
            let SimulatorToSurferMessage::ValueChanges { time_steps, status } =
                decode_simulator_to_surfer_message(encoded).unwrap()
            else {
                panic!("wrong message type")
            };
            assert_eq!(time_steps.len(), 1);
            assert_eq!(time_steps[0].timestamp, index as u64);
            assert_eq!(
                status,
                if index == 2 {
                    SimulationStatus::Done
                } else {
                    SimulationStatus::Running
                }
            );
        }
    }

    #[test]
    fn empty_terminal_updates_reach_the_queue() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let connection = ClientConnection::new(tx);
        for status in [SimulationStatus::Paused, SimulationStatus::Done] {
            connection.send_value_changes(Vec::new(), status);
            let SimulatorToSurferMessage::ValueChanges {
                time_steps,
                status: decoded_status,
            } = decode_simulator_to_surfer_message(&rx.try_recv().unwrap()).unwrap()
            else {
                panic!("wrong message type")
            };
            assert!(time_steps.is_empty());
            assert_eq!(decoded_status, status);
        }
        connection.send_value_changes(Vec::new(), SimulationStatus::Running);
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn uint_to_bit_string_basic_and_order() {
        assert_eq!(uint_to_bit_string(0b1010u64, 4), "1010");
        assert_eq!(uint_to_bit_string(1u64, 4), "0001");
        assert_eq!(uint_to_bit_string(0u64, 4), "0000");
    }

    #[test]
    fn uint_to_bit_string_truncates_to_width() {
        assert_eq!(uint_to_bit_string(0b1_0110u64, 4), "0110");
        assert_eq!(uint_to_bit_string(u64::MAX, 8), "11111111");
    }

    #[test]
    fn uint_to_bit_string_zero_width() {
        assert_eq!(uint_to_bit_string(123u64, 0), "");
    }

    #[test]
    fn int_to_bit_string_positive_values() {
        assert_eq!(int_to_bit_string(0b1010i64, 4), "1010");
        assert_eq!(int_to_bit_string(3i64, 6), "000011");
    }

    #[test]
    fn int_to_bit_string_negative_values_use_twos_complement_bits() {
        assert_eq!(int_to_bit_string(-1i64, 4), "1111");
        assert_eq!(int_to_bit_string(-2i64, 4), "1110");
        assert_eq!(int_to_bit_string(-8i64, 4), "1000");
    }

    #[test]
    fn int_to_bit_string_truncates_to_width() {
        assert_eq!(int_to_bit_string(0b1_1010i64, 4), "1010");
        assert_eq!(int_to_bit_string(-1i64, 1), "1");
    }

    #[test]
    fn int_to_bit_string_zero_width() {
        assert_eq!(int_to_bit_string(-42i64, 0), "");
    }
}
