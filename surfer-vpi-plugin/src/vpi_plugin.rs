use portable_atomic::AtomicU128;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Instant;
use surfer_simulator_protocol::plugin::{
    ClientConnection, SimulatorCommand, SimulatorData, StartupOptions, ValueChangeBuffer,
    int_to_bit_string, uint_to_bit_string,
};
use surfer_simulator_protocol::{
    HierarchyElement, ScopeType, SignalEncoding, SignalValue, SimId, SimulationStatus,
    SimulationTimestamp, SimulatorToSurferMessage, SurferToSimulatorMessage, VarDirection,
    VarIndex, VarType,
};
use tokio::sync::mpsc;
use vpi::{CbData, CbReason, Control, Direction, Handle, ObjectType, Time, Value, ValueType};

// The simulator thread owns all VPI calls. Tokio workers handle socket I/O;
// inbound simulator requests are sent back through this command queue.
static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static SIMULATOR_THREAD_ID: OnceLock<thread::ThreadId> = OnceLock::new();
static SIMULATOR_COMMAND_SENDER: OnceLock<mpsc::Sender<SimulatorCommand>> = OnceLock::new();
static SIMULATOR_COMMAND_RECEIVER: OnceLock<Mutex<mpsc::Receiver<SimulatorCommand>>> =
    OnceLock::new();
static SIMULATOR_DATA: OnceLock<Arc<StaticSimData>> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<ValueChangeBuffer>> = OnceLock::new();
static TIMEOUT_CALLBACK: OnceLock<Mutex<Option<SafeHandle>>> = OnceLock::new();
static LAST_SEND_TIME_MS: AtomicU128 = AtomicU128::new(0);
static LAST_NEXTSIM_CB_TIME: AtomicU64 = AtomicU64::new(u64::MAX);
static CALLBACK_STORE: OnceLock<Arc<CallbackStore>> = OnceLock::new();
static SIMULATION_DONE: AtomicBool = AtomicBool::new(false);
static SIMULATION_PAUSED: AtomicBool = AtomicBool::new(false);
static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

// Icarus sets name for unnamed objects to "unnamed", so we use that as fallback name.
const UNNAMED: &str = "unnamed";

// Time in ms between when plugin sends simulation time update to Surfer.
const INTERVAL_MS: u128 = 500;

fn is_simulator_thread() -> bool {
    SIMULATOR_THREAD_ID
        .get()
        .is_some_and(|owner| *owner == thread::current().id())
}

fn require_simulator_thread(operation: &str) -> bool {
    if is_simulator_thread() {
        true
    } else {
        eprintln!("Refusing VPI operation outside simulator thread: {operation}");
        false
    }
}

fn plugin_message(message: impl AsRef<str>) {
    // Unit tests run without a simulator to provide the Linux vpi_printf symbol.
    #[cfg(not(test))]
    if is_simulator_thread() {
        vpi::printf(message.as_ref());
        return;
    }
    eprintln!("{}", message.as_ref());
}

fn debug_message(message: impl AsRef<str>) {
    if DEBUG_ENABLED.load(Ordering::Relaxed) {
        plugin_message(format!("[debug] {}", message.as_ref()));
    }
}

#[derive(PartialEq, Eq)]
enum CommandDisposition {
    Continue,
    ResumeSimulation,
    Finish,
}

// Simulator callbacks enqueue encoded updates here; the Tokio connection task
// drains them and writes them to Surfer.
static CLIENT_CONNECTION: OnceLock<ClientConnection> = OnceLock::new();

#[inline]
fn now_millis() -> u128 {
    // Measure update intervals independently of changes to the system clock.
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis()
}

type StaticSimData = SimulatorData<HashMap<SimId, SafeHandle>>;

struct SendHandle(Handle);

// VPI handles are not Send. This wrapper permits shared storage, while
// SafeHandle checks that access remains on the thread that created the handle.
unsafe impl Send for SendHandle {}

pub struct SafeHandle {
    owner_thread: thread::ThreadId,
    handle: Mutex<SendHandle>,
}

impl SafeHandle {
    fn from_handle(handle: Handle) -> Self {
        Self {
            owner_thread: thread::current().id(),
            handle: Mutex::new(SendHandle(handle)),
        }
    }

    fn with_handle<Output>(&self, operation: impl FnOnce(&Handle) -> Output) -> Option<Output> {
        if self.owner_thread != thread::current().id() {
            plugin_message("Refusing VPI handle access from non-owner thread");
            return None;
        }
        let Ok(handle) = self.handle.lock() else {
            plugin_message("Failed to lock VPI handle");
            return None;
        };
        Some(operation(&handle.0))
    }
}

type CallbackStore = surfer_simulator_protocol::plugin::CallbackStore<SafeHandle>;

/// Resolve ports and collapsed nets without inferring equivalence from connectivity.
/// Expressions and partial selections retain their own identities.
fn canonical_signal_handle(mut handle: Handle) -> Handle {
    if handle.is_port() {
        let internal = handle.get(ObjectType::LowConn);
        let internal = if internal.is_null() {
            // Icarus exposes the internal declaration by scope/name instead.
            sibling_value_handle_for_port(&handle)
        } else {
            internal
        };
        if !internal.is_null()
            && internal.get_size() == handle.get_size()
            && matches!(internal.get_type(), Some(ObjectType::Net | ObjectType::Reg))
        {
            handle = internal;
        }
    }
    if matches!(
        handle.get_type(),
        Some(ObjectType::Net | ObjectType::NetBit)
    ) {
        let simulated = handle.get(ObjectType::SimNet);
        if !simulated.is_null()
            && simulated.get_size() == handle.get_size()
            && matches!(
                simulated.get_type(),
                Some(ObjectType::Net | ObjectType::NetBit)
            )
        {
            return simulated;
        }
    }
    handle
}

// Unlike the VPI shim, optional lookup must not abort when an older simulator
// lacks vpi_get64. The legacy vpi_get property truncates pointers on 64-bit hosts.
fn icarus_nexus_id(handle: &Handle) -> Option<i64> {
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    {
        type Get64 = unsafe extern "C" fn(i32, *mut std::ffi::c_void) -> i64;
        static GET64: OnceLock<Option<Get64>> = OnceLock::new();
        let get64 = GET64.get_or_init(|| {
            #[cfg(windows)]
            let host = libloading::os::windows::Library::this().ok()?;
            #[cfg(unix)]
            let host = libloading::os::unix::Library::this();
            // SAFETY: resolve the standard VPI function with its C signature.
            // The host executable remains loaded for the lifetime of the process.
            unsafe { host.get::<Get64>(b"vpi_get64\0").ok().map(|symbol| *symbol) }
        });
        let get64 = (*get64)?;
        // SAFETY: called only for Icarus whole signals on the simulator thread.
        let nexus = unsafe { get64(0x1000000, handle.as_raw().cast()) };
        (nexus > 0).then_some(nexus)
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        // On macOS a process-wide lookup may find our own aborting shim rather
        // than the host's implementation, so retain the standard identity path.
        let _ = handle;
        None
    }
}

/// Alias resolution is independent of simulator-owned handles and their cleanup.
#[derive(Default)]
struct IdAliases {
    parents: HashMap<SimId, SimId>,
}

impl IdAliases {
    fn is_alias(&self, id: SimId) -> bool {
        self.parents.contains_key(&id)
    }

    fn resolve_id(&self, mut id: SimId) -> SimId {
        while let Some(parent) = self.parents.get(&id) {
            id = *parent;
        }
        id
    }

    fn merge_ids(&mut self, left: SimId, right: SimId) {
        let left = self.resolve_id(left);
        let right = self.resolve_id(right);
        if left != right {
            self.parents.insert(left.max(right), left.min(right));
        }
    }
}

pub struct IdStore {
    next_id: AtomicU32,
    aliases: IdAliases,
    is_icarus: Option<bool>,
    nexus_ids: HashMap<(i64, SignalEncoding), SimId>,
    // Canonical full names only narrow the search; handle comparison establishes identity.
    identities: HashMap<(Option<String>, SignalEncoding), Vec<SimId>>,
    pub id_to_handle: HashMap<SimId, SafeHandle>,
}

impl IdStore {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(0),
            aliases: IdAliases::default(),
            is_icarus: None,
            nexus_ids: HashMap::new(),
            identities: HashMap::new(),
            id_to_handle: HashMap::new(),
        }
    }

    pub fn create_id(&mut self, handle: Handle, encoding: SignalEncoding) -> SimId {
        let handle = canonical_signal_handle(handle);
        let is_icarus = *self.is_icarus.get_or_insert_with(|| {
            vpi::try_simulator_name().is_ok_and(|name| name.to_ascii_lowercase().contains("icarus"))
        });
        let nexus = if is_icarus
            && encoding == SignalEncoding::BitVector(1)
            && matches!(handle.get_type(), Some(ObjectType::Net | ObjectType::Reg))
        {
            // The same identity extension is used by Icarus's VCD writer.
            icarus_nexus_id(&handle)
        } else {
            None
        };
        if let Some(id) = nexus.and_then(|nexus| self.nexus_ids.get(&(nexus, encoding))) {
            return self.resolve_id(*id);
        }
        let key = (handle.get_full_name(), encoding);
        if let Some(candidates) = self.identities.get(&key) {
            for id in candidates {
                if self.id_to_handle.get(id).is_some_and(|existing| {
                    existing.with_handle(|existing| existing == &handle) == Some(true)
                }) {
                    return self.resolve_id(*id);
                }
            }
        }
        let id: SimId = self.next_id.fetch_add(1, Ordering::Relaxed);

        self.id_to_handle
            .insert(id, SafeHandle::from_handle(handle));
        self.identities.entry(key).or_default().push(id);
        if let Some(nexus) = nexus {
            self.nexus_ids.insert((nexus, encoding), id);
        }
        id
    }

    fn resolve_id(&self, id: SimId) -> SimId {
        self.aliases.resolve_id(id)
    }

    fn merge_ids(&mut self, left: SimId, right: SimId) {
        self.aliases.merge_ids(left, right);
    }
}

#[inline]
fn current_sim_time() -> u64 {
    vpi::current_simulation_time().to_u64().unwrap_or(0)
}

#[inline]
fn sim_time_from_callback(cb_time: Option<u64>) -> u64 {
    cb_time.unwrap_or_else(current_sim_time)
}

fn infer_var_type_and_encoding(signal: &Handle, fallback_bits: u32) -> (VarType, SignalEncoding) {
    let fallback_bits = fallback_bits.max(1);

    match signal.get_type() {
        Some(ObjectType::Port | ObjectType::PortBit) => {
            return (VarType::Port, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::Net | ObjectType::NetBit) => {
            return (VarType::Wire, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::Reg | ObjectType::RegBit) => {
            return (VarType::Reg, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::NamedEvent) => {
            return (VarType::Event, SignalEncoding::BitVector(0));
        }
        Some(ObjectType::IntVar) => return (VarType::Int, SignalEncoding::BitVector(32)),
        Some(ObjectType::IntegerVar) => {
            return (VarType::Integer, SignalEncoding::BitVector(32));
        }
        Some(ObjectType::ShortIntVar) => {
            return (VarType::ShortInt, SignalEncoding::BitVector(16));
        }
        Some(ObjectType::LongIntVar) => {
            return (VarType::LongInt, SignalEncoding::BitVector(64));
        }
        Some(ObjectType::TimeVar) => {
            return (VarType::Time, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::ByteVar) => return (VarType::Byte, SignalEncoding::BitVector(8)),
        Some(ObjectType::BitVar) => {
            let var_type = if fallback_bits == 1 {
                VarType::Bit
            } else {
                VarType::BitVector
            };
            return (var_type, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::EnumVar) => {
            return (VarType::Enum, SignalEncoding::BitVector(fallback_bits));
        }
        Some(ObjectType::StringVar) => return (VarType::String, SignalEncoding::String),
        Some(ObjectType::ShortRealVar) => return (VarType::ShortReal, SignalEncoding::Real),
        Some(ObjectType::RealVar) => return (VarType::Real, SignalEncoding::Real),
        Some(ObjectType::MemoryWord) => {
            // Many simulators report memory words via ObjType values as Int,
            // even when the declared element width is not 32. Prefer width-based
            // vector typing for memory elements to preserve declared bit widths.
            let var_type = if fallback_bits == 1 {
                VarType::Bit
            } else {
                VarType::BitVector
            };
            return (var_type, SignalEncoding::BitVector(fallback_bits));
        }
        _ => {}
    }

    match signal.get_value(ValueType::ObjType) {
        Some(Value::Scalar(_)) => (VarType::Wire, SignalEncoding::BitVector(1)),
        Some(Value::Vector(vals)) => {
            let bits = vals.len().max(1) as u32;
            (VarType::Wire, SignalEncoding::BitVector(bits))
        }
        Some(Value::Real(_) | Value::ShortReal(_)) => (VarType::Real, SignalEncoding::Real),
        Some(Value::String(_)) => (VarType::String, SignalEncoding::String),
        Some(Value::Int(_)) => {
            if fallback_bits == 32 {
                (VarType::Int, SignalEncoding::BitVector(32))
            } else {
                (VarType::Wire, SignalEncoding::BitVector(fallback_bits))
            }
        }
        Some(Value::ShortInt(_)) => {
            if fallback_bits == 16 {
                (VarType::ShortInt, SignalEncoding::BitVector(16))
            } else {
                (VarType::Wire, SignalEncoding::BitVector(fallback_bits))
            }
        }
        Some(Value::LongInt(_)) => {
            if fallback_bits == 64 {
                (VarType::LongInt, SignalEncoding::BitVector(64))
            } else {
                (VarType::Wire, SignalEncoding::BitVector(fallback_bits))
            }
        }
        Some(Value::Time(_)) => (VarType::Time, SignalEncoding::BitVector(fallback_bits)),
        _ => (VarType::Wire, SignalEncoding::BitVector(fallback_bits)),
    }
}

fn add_signal_var_named(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    signal: Handle,
    name: String,
) {
    let type_name = signal
        .get_type_name()
        .unwrap_or_else(|| "Unknown".to_string());

    // Icarus errors on vpiDirection for non-port objects; only ask direction on ports.
    let direction = if signal.is_port() {
        match signal.get_direction().as_ref() {
            Some(Direction::Input) => Some(VarDirection::Input),
            Some(Direction::Output) => Some(VarDirection::Output),
            Some(Direction::Inout) => Some(VarDirection::InOut),
            _ => None,
        }
    } else {
        None
    };
    let bits = signal.get_size().unwrap_or(1).max(1);
    let (var_type, signal_encoding) = infer_var_type_and_encoding(&signal, bits);

    let index = match &signal_encoding {
        SignalEncoding::BitVector(width) if *width > 1 => Some(VarIndex {
            msb: i64::from(*width) - 1,
            lsb: 0,
        }),
        _ => None,
    };

    let id = id_store.create_id(signal, signal_encoding);

    debug_message(format!(
        "hierarchy signal: id={id:?}, name={name:?}, type={type_name:?}, var_type={var_type:?}, encoding={signal_encoding:?}, direction={direction:?}, bits={bits}"
    ));

    h.push(HierarchyElement::AddVar {
        id,
        name,
        type_name,
        var_type,
        signal_encoding,
        var_direction: direction,
        enum_map: None,
        index,
        is_parameter: false,
    });
}

#[inline]
fn add_signal_var(h: &mut Vec<HierarchyElement>, id_store: &mut IdStore, signal: Handle) {
    let name = signal.get_name().unwrap_or_else(|| UNNAMED.to_string());
    add_signal_var_named(h, id_store, signal, name);
}

#[inline]
fn signal_display_name(signal: &Handle) -> Option<String> {
    signal
        .get_name()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && name != UNNAMED)
}

#[inline]
fn get_signal_full_name(signal: &Handle) -> Option<String> {
    signal
        .get_full_name()
        .or_else(|| signal_display_name(signal))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && name != UNNAMED)
}

#[inline]
fn is_array_object_type(object_type: Option<ObjectType>) -> bool {
    matches!(
        object_type,
        Some(
            ObjectType::Memory
                | ObjectType::RegArray
                | ObjectType::NetArray
                | ObjectType::NamedEventArray
                | ObjectType::PackedArrayNet
                | ObjectType::PackedArrayVar
        )
    )
}

fn infer_ranges_from_name(base_full_name: &str, child_count: usize) -> Vec<(i32, i32)> {
    if child_count == 0 {
        return Vec::new();
    }

    // Determine how many unpacked index brackets are addressable via handle-by-name.
    let mut depth = 1usize;
    while depth <= 8 {
        let probe = format!("{base_full_name}{}", "[0]".repeat(depth));
        if Handle::handle_by_name(&probe).is_null() {
            break;
        }
        depth += 1;
    }
    let max_depth = depth.saturating_sub(1);
    if max_depth <= 1 {
        return Vec::new();
    }

    let mut extents = Vec::with_capacity(max_depth);
    for dim in 0..max_depth {
        let mut idx = 0usize;
        loop {
            let mut name = String::from(base_full_name);
            for d in 0..max_depth {
                let value = if d == dim { idx } else { 0 };
                name.push_str(&format!("[{value}]"));
            }
            if Handle::handle_by_name(&name).is_null() {
                break;
            }
            idx += 1;
            if idx > child_count {
                break;
            }
        }

        if idx == 0 {
            return Vec::new();
        }
        extents.push(idx);
    }

    let product = extents.iter().copied().try_fold(1usize, usize::checked_mul);
    if product != Some(child_count) {
        return Vec::new();
    }

    extents
        .into_iter()
        .map(|extent| (0, extent.saturating_sub(1) as i32))
        .collect()
}

fn array_ranges(signal: &Handle) -> Vec<(i32, i32)> {
    let mut ranges: Vec<(i32, i32)> = signal
        .iterator(ObjectType::Range)
        .filter_map(|range| {
            let left = range.get_left_range()?;
            let right = range.get_right_range()?;
            Some((left, right))
        })
        .collect();

    if !ranges.is_empty() {
        return ranges;
    }

    // Some simulators expose only a direct vpiRange relation (and potentially
    // chained range relations) rather than returning range iterators.
    let mut current = signal.get(ObjectType::Range);
    let mut guard = 0usize;
    while !current.is_null() && guard < 16 {
        if let (Some(left), Some(right)) = (current.get_left_range(), current.get_right_range()) {
            ranges.push((left, right));
        }
        current = current.get(ObjectType::Range);
        guard += 1;
    }

    if !ranges.is_empty() {
        return ranges;
    }

    // Final fallback: derive unpacked dimensions from SystemVerilog type metadata.
    // For arrays like logic [3:0][1:0][7:0] mixed_3d [0:1][0:2], the Memory handle
    // may expose only flattened MemoryWord children; unpacked dimensions can still
    // be represented as ArrayTypespec nodes in the ElemTypespec chain.
    let mut typespec = signal.get(ObjectType::Typespec);
    let mut type_guard = 0usize;
    while !typespec.is_null() && type_guard < 32 {
        if matches!(typespec.get_type(), Some(ObjectType::ArrayTypespec)) {
            let mut type_ranges: Vec<(i32, i32)> = typespec
                .iterator(ObjectType::Range)
                .filter_map(|range| {
                    let left = range.get_left_range()?;
                    let right = range.get_right_range()?;
                    Some((left, right))
                })
                .collect();

            if type_ranges.is_empty() {
                let direct_type_range = typespec.get(ObjectType::Range);
                if let (Some(left), Some(right)) = (
                    direct_type_range.get_left_range(),
                    direct_type_range.get_right_range(),
                ) {
                    type_ranges.push((left, right));
                }
            }

            ranges.extend(type_ranges);
        }

        let next = typespec.get(ObjectType::ElemTypespec);
        if next.is_null() {
            break;
        }
        typespec = next;
        type_guard += 1;
    }

    ranges
}

fn linear_index_to_dim_indices(linear_index: usize, ranges: &[(i32, i32)]) -> Option<Vec<i32>> {
    if ranges.is_empty() {
        return None;
    }

    let extents: Vec<usize> = ranges
        .iter()
        .map(|(left, right)| (*left - *right).unsigned_abs() as usize + 1)
        .collect();

    let total_elems = extents
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)?;
    if linear_index >= total_elems {
        return None;
    }

    let mut remaining = linear_index;
    let mut indices = Vec::with_capacity(ranges.len());

    for dim in 0..ranges.len() {
        let stride = extents[dim + 1..]
            .iter()
            .copied()
            .fold(1usize, usize::saturating_mul);
        let offset = (remaining.checked_div(stride).unwrap_or(0)) % extents[dim];

        let (left, right) = ranges[dim];
        let index = if left <= right {
            left + offset as i32
        } else {
            left - offset as i32
        };
        indices.push(index);

        if stride > 0 {
            remaining %= stride;
        }
    }

    Some(indices)
}

fn cancel_active_callback() {
    let Ok(mut active_callback) = TIMEOUT_CALLBACK.get_or_init(|| Mutex::new(None)).lock() else {
        plugin_message("Failed to lock timeout callback state");
        return;
    };

    if let Some(callback_handle) = active_callback.take() {
        let _ = callback_handle.with_handle(vpi::remove_cb);
    }
}

fn store_active_callback(callback_handle: Handle) {
    let Ok(mut active_callback) = TIMEOUT_CALLBACK.get_or_init(|| Mutex::new(None)).lock() else {
        plugin_message("Failed to lock timeout callback state after registration");
        vpi::remove_cb(&callback_handle);
        return;
    };
    *active_callback = Some(SafeHandle::from_handle(callback_handle));
}

fn register_timeout_callback(delay: u64) {
    cancel_active_callback();
    let callback_handle =
        vpi::register_cb_with_time(CbReason::AfterDelay, Time::Sim(delay), timeout_callback);
    store_active_callback(callback_handle);
}

fn register_end_of_sim_time_callback() {
    cancel_active_callback();
    let callback_handle = vpi::register_cb(CbReason::AtEndOfSimTime, timeout_callback);
    store_active_callback(callback_handle);
}

fn array_child_display_name(
    child: &Handle,
    fallback_index: usize,
    parent_ranges: &[(i32, i32)],
) -> String {
    let linear_index = child
        .get_index()
        .and_then(|index| usize::try_from(index).ok())
        .unwrap_or(fallback_index);

    if let Some(dim_indices) = linear_index_to_dim_indices(linear_index, parent_ranges) {
        let mut name = String::new();
        for idx in dim_indices {
            let _ = write!(&mut name, "[{idx}]");
        }
        name
    } else {
        format!("[{linear_index}]")
    }
}

fn preferred_callback_value_type(signal: &Handle) -> ValueType {
    let bits = signal.get_size().unwrap_or(1).max(1);
    let object_type = signal.get_type();
    let inferred_from_type = match object_type {
        // These do not work on Icarus Verilog, so we comment them out for now. They may work on other simulators.
        // Some(ObjectType::ShortIntVar) => Some(ValueType::ShortInt),
        // Some(ObjectType::LongIntVar) => Some(ValueType::LongInt),
        Some(ObjectType::ShortRealVar) => Some(ValueType::ShortReal),
        Some(ObjectType::RealVar) => Some(ValueType::Real),
        Some(ObjectType::StringVar) => Some(ValueType::String),
        Some(ObjectType::IntVar | ObjectType::IntegerVar) => Some(ValueType::Int),
        Some(ObjectType::TimeVar) => Some(ValueType::Time),
        Some(ObjectType::ByteVar) => Some(ValueType::Vector),
        Some(
            ObjectType::Port
            | ObjectType::PortBit
            | ObjectType::Net
            | ObjectType::NetBit
            | ObjectType::Reg
            | ObjectType::RegBit
            | ObjectType::Bit
            | ObjectType::BitVar
            | ObjectType::EnumVar
            | ObjectType::NamedEvent
            | ObjectType::MemoryWord
            | ObjectType::PackedArrayNet
            | ObjectType::PackedArrayVar
            | ObjectType::PartSelect
            | ObjectType::IndexedPartSelect
            | ObjectType::VarSelect,
        ) => {
            if bits == 1 {
                Some(ValueType::Scalar)
            } else {
                Some(ValueType::Vector)
            }
        }
        _ => None,
    };

    if let Some(value_type) = inferred_from_type {
        return value_type;
    }
    match signal.get_value(ValueType::ObjType) {
        Some(Value::Scalar(_)) => ValueType::Scalar,
        Some(Value::Vector(_)) => ValueType::Vector,
        Some(Value::Real(_)) => ValueType::Real,
        Some(Value::ShortReal(_)) => ValueType::ShortReal,
        Some(Value::String(_)) => ValueType::String,
        Some(Value::Int(_)) => ValueType::Int,
        Some(Value::ShortInt(_)) => ValueType::ShortInt,
        Some(Value::LongInt(_)) => ValueType::LongInt,
        Some(Value::Time(_)) => ValueType::Time,
        Some(Value::BinStr(_)) => ValueType::BinStr,
        Some(Value::OctStr(_)) => ValueType::OctStr,
        Some(Value::HexStr(_)) => ValueType::HexStr,
        Some(Value::DecStr(_)) => ValueType::DecStr,
        Some(Value::Strength(_)) => ValueType::Strength,
        Some(Value::ObjType(_) | Value::Suppress) | None => {
            let bits = signal.get_size().unwrap_or(1).max(1);
            if bits == 1 {
                ValueType::Scalar
            } else {
                ValueType::Vector
            }
        }
    }
}

fn collect_signal_object(
    signal: Handle,
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    seen_signals: &mut HashSet<String>,
    declaration_scope: Option<&str>,
) {
    let object_type = signal.get_type();
    let Some(signal_name) = get_signal_full_name(&signal) else {
        debug_message(format!(
            "hierarchy object skipped: no full name, object_type={object_type:?}"
        ));
        return;
    };
    // Some simulators return only the local name for port handles.
    // Qualify the declaration key so equal names in different scopes survive.
    let declaration_key = declaration_scope.map_or_else(
        || signal_name.clone(),
        |scope| format!("{scope}.{signal_name}"),
    );
    if !seen_signals.insert(declaration_key) {
        debug_message(format!(
            "hierarchy object skipped as duplicate: {signal_name:?}"
        ));
        return;
    }

    debug_message(format!(
        "collecting hierarchy object: name={signal_name:?}, object_type={object_type:?}"
    ));

    if !is_array_object_type(object_type) {
        debug_message(format!("adding scalar hierarchy object: {signal_name:?}"));
        add_signal_var(h, id_store, signal);
        return;
    }

    let signal_full_name = signal_name;

    let children: Vec<Handle> = match object_type {
        Some(ObjectType::Memory) => signal.iterator(ObjectType::MemoryWord).collect(),
        Some(ObjectType::RegArray) => signal
            .iterators(&[ObjectType::RegBit, ObjectType::Reg])
            .collect(),
        Some(ObjectType::NetArray) => signal
            .iterators(&[ObjectType::NetBit, ObjectType::Net])
            .collect(),
        Some(ObjectType::NamedEventArray) => signal.iterator(ObjectType::NamedEvent).collect(),
        Some(ObjectType::PackedArrayNet) => signal
            .iterators(&[
                ObjectType::NetBit,
                ObjectType::Net,
                ObjectType::Bit,
                ObjectType::VarSelect,
                ObjectType::PartSelect,
                ObjectType::IndexedPartSelect,
            ])
            .collect(),
        Some(ObjectType::PackedArrayVar) => signal
            .iterators(&[
                ObjectType::BitVar,
                ObjectType::RegBit,
                ObjectType::Reg,
                ObjectType::Bit,
                ObjectType::VarSelect,
                ObjectType::PartSelect,
                ObjectType::IndexedPartSelect,
            ])
            .collect(),
        _ => Vec::new(),
    };
    let mut ranges = array_ranges(&signal);
    debug_message(format!(
        "array hierarchy object: name={signal_full_name:?}, children={}, ranges={ranges:?}",
        children.len()
    ));
    if ranges.is_empty() {
        let inferred = infer_ranges_from_name(&signal_full_name, children.len());
        if !inferred.is_empty() {
            ranges = inferred;
            debug_message(format!(
                "inferred array ranges: name={signal_full_name:?}, ranges={ranges:?}"
            ));
        }
    }

    if children.is_empty() {
        debug_message(format!(
            "array has no children; adding as a signal: {signal_full_name:?}"
        ));
        add_signal_var(h, id_store, signal);
        return;
    }

    let name = signal_display_name(&signal).unwrap_or_else(|| UNNAMED.to_string());

    debug_message(format!(
        "entered array scope: name={signal_full_name:?}, display_name={name:?}"
    ));
    h.push(HierarchyElement::PushScope {
        name,
        scope_type: ScopeType::SvArray,
    });

    for (fallback_index, child) in children.into_iter().enumerate() {
        let child_is_array = is_array_object_type(child.get_type());

        if !child_is_array && get_signal_full_name(&child).is_none() && child.get_index().is_none()
        {
            debug_message(format!(
                "array child skipped: parent={signal_full_name:?}, fallback_index={fallback_index}, no name or index"
            ));
            continue;
        }

        if child_is_array {
            debug_message(format!(
                "descending into nested array: parent={signal_full_name:?}, child_type={:?}",
                child.get_type()
            ));
            collect_signal_object(child, h, id_store, seen_signals, None);
        } else {
            let display_name = array_child_display_name(&child, fallback_index, &ranges);
            debug_message(format!(
                "adding array child: parent={signal_full_name:?}, display_name={display_name:?}, fallback_index={fallback_index}, child_index={:?}",
                child.get_index()
            ));
            add_signal_var_named(h, id_store, child, display_name);
        }
    }

    h.push(HierarchyElement::PopScope);
    debug_message(format!("leaving array scope: name={signal_full_name:?}"));
}

fn collect_hierarchy_elements_from_scope(
    scope: &Handle,
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    seen_signals: &mut HashSet<String>,
) {
    let scope_name = scope.get_name().unwrap_or_else(|| UNNAMED.to_string());

    debug_message(format!(
        "entering module hierarchy scope: name={scope_name:?}, object_type={:?}",
        scope.get_type()
    ));

    h.push(HierarchyElement::PushScope {
        name: scope_name.clone(),
        scope_type: ScopeType::Module,
    });

    // Explicit SystemVerilog aliases are identity declarations, unlike ordinary
    // port connections or continuous assignments. Only fold whole nets here.
    for alias in scope.iterator(ObjectType::AliasStmt) {
        let left = alias.get(ObjectType::Lhs);
        let right = alias.get(ObjectType::Rhs);
        if left.is_null() || right.is_null() {
            continue;
        }
        if matches!(left.get_type(), Some(ObjectType::Net))
            && matches!(right.get_type(), Some(ObjectType::Net))
        {
            let (_, left_encoding) =
                infer_var_type_and_encoding(&left, left.get_size().unwrap_or(1).max(1));
            let (_, right_encoding) =
                infer_var_type_and_encoding(&right, right.get_size().unwrap_or(1).max(1));
            if left_encoding == right_encoding {
                let left = id_store.create_id(left, left_encoding);
                let right = id_store.create_id(right, right_encoding);
                id_store.merge_ids(left, right);
            }
        }
    }

    let scope_full_name = scope.get_full_name().unwrap_or_else(|| scope_name.clone());
    let mut port_names_in_scope = HashSet::new();

    let ports: Vec<Handle> = scope
        .iterators(&[ObjectType::Port, ObjectType::PortBit])
        .collect();
    debug_message(format!(
        "scope ports: name={scope_name:?}, count={}",
        ports.len()
    ));
    for signal in ports {
        if let Some(name) = signal_display_name(&signal) {
            port_names_in_scope.insert(name);
        }
        collect_signal_object(signal, h, id_store, seen_signals, Some(&scope_full_name));
    }

    let signals: Vec<Handle> = scope
        .iterators(&[
            ObjectType::Net,
            ObjectType::NetArray,
            ObjectType::Reg,
            ObjectType::Memory,
            ObjectType::RegArray,
            ObjectType::NamedEvent,
            ObjectType::NamedEventArray,
            ObjectType::PackedArrayNet,
            ObjectType::Variables,
        ])
        .collect();
    debug_message(format!(
        "scope signals: name={scope_name:?}, count={}",
        signals.len()
    ));
    for signal in signals {
        let Some(full_name) = get_signal_full_name(&signal) else {
            debug_message(format!(
                "scope signal skipped: scope={scope_name:?}, object_type={:?}, no full name",
                signal.get_type()
            ));
            continue;
        };

        if !signal.is_port()
            && signal_display_name(&signal)
                .as_ref()
                .is_some_and(|name| port_names_in_scope.contains(name))
        {
            debug_message(format!(
                "scope signal skipped as port duplicate: scope={scope_name:?}, name={full_name:?}"
            ));
            continue;
        }

        debug_message(format!(
            "collecting scope signal: scope={scope_name:?}, name={full_name:?}, object_type={:?}",
            signal.get_type()
        ));
        collect_signal_object(signal, h, id_store, seen_signals, Some(&scope_full_name));
    }

    let child_scopes: Vec<Handle> = scope
        .iterators(&[
            ObjectType::Module,
            ObjectType::Interface,
            ObjectType::Program,
            ObjectType::Package,
            ObjectType::ClassDefn,
            ObjectType::GenScope,
            ObjectType::NamedBegin,
            ObjectType::NamedFork,
            ObjectType::InternalScope,
        ])
        .collect();
    debug_message(format!(
        "scope child scopes: name={scope_name:?}, count={}",
        child_scopes.len()
    ));
    let mut seen_child_scopes = HashSet::new();
    for child_scope in child_scopes {
        if let Some(name) = child_scope.get_full_name() {
            if !seen_child_scopes.insert(name) {
                continue;
            }
        }
        collect_hierarchy_elements_from_scope(&child_scope, h, id_store, seen_signals);
    }

    h.push(HierarchyElement::PopScope);
    debug_message(format!(
        "leaving module hierarchy scope: name={scope_name:?}"
    ));
}

fn collect_hierarchy() -> (Vec<HierarchyElement>, HashMap<SimId, SafeHandle>) {
    let mut hierarchy = Vec::new();
    let mut id_store = IdStore::new();
    let mut seen_signals = HashSet::new();
    let root = Handle::null();

    let top_scopes: Vec<Handle> = root
        .iterators(&[
            ObjectType::Module,
            ObjectType::Interface,
            ObjectType::Program,
            ObjectType::Package,
        ])
        .collect();
    debug_message(format!(
        "top-level hierarchy scopes: count={}",
        top_scopes.len()
    ));

    for module in top_scopes {
        collect_hierarchy_elements_from_scope(
            &module,
            &mut hierarchy,
            &mut id_store,
            &mut seen_signals,
        );
    }

    // Aliases may be declared after a target was visited, including in another
    // scope. Resolve all references before exposing the hierarchy or callbacks.
    for element in &mut hierarchy {
        if let HierarchyElement::AddVar { id, .. } = element {
            *id = id_store.resolve_id(*id);
        }
    }
    let aliases = &id_store.aliases;
    id_store.id_to_handle.retain(|id, _| !aliases.is_alias(*id));
    debug_message(format!(
        "collected hierarchy: {} elements, {} signals",
        hierarchy.len(),
        id_store.id_to_handle.len()
    ));

    (hierarchy, id_store.id_to_handle)
}

fn timeout_callback(_data: &CbData) {
    if !require_simulator_thread("timeout callback") {
        return;
    }

    // The callback fired, so it is no longer an active timeout to cancel.
    if let Ok(mut active_callback) = TIMEOUT_CALLBACK.get_or_init(|| Mutex::new(None)).lock() {
        active_callback.take();
    }

    plugin_message("Timeout reached. Sending values and pausing...");
    SIMULATION_PAUSED.store(true, Ordering::SeqCst);
    {
        let Ok(mut value_changes) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            plugin_message("Failed to lock VALUE_CHANGES mutex while pausing");
            return;
        };

        value_changes.ensure_time_step(SimulationTimestamp::from_ticks(current_sim_time()));
    }
    send_changed_values(SimulationStatus::Paused);

    if !wait_for_run_command() {
        return;
    }
    SIMULATION_PAUSED.store(false, Ordering::SeqCst);
    plugin_message("Simulation resumed!");
}

fn send_changed_values(status: SimulationStatus) {
    let value_changes = {
        let Ok(mut global_map) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            plugin_message("Failed to lock VALUE_CHANGES mutex while sending values");
            return;
        };

        global_map.take()
    };

    if let Some(connection) = CLIENT_CONNECTION.get() {
        connection.send_value_changes(value_changes, status);
    }
}

fn start_of_sim(_data: &CbData) {
    if !require_simulator_thread("start-of-simulation callback") {
        return;
    }

    SIMULATION_DONE.store(false, Ordering::SeqCst);
    SIMULATION_PAUSED.store(false, Ordering::SeqCst);
    LAST_NEXTSIM_CB_TIME.store(u64::MAX, Ordering::Relaxed);

    let (bind_address, no_info) = get_startup_argument_values();
    debug_message("Start of simulation entered");
    let (hierarchy, id_to_handle) = collect_hierarchy();
    let data = Arc::new(StaticSimData {
        hierarchy,
        id_to_handle: Arc::new(id_to_handle),
        simulator_name: vpi::simulator_name(),
        simulator_version: vpi::simulator_version(),
        time_precision: vpi::get_simulator_precision(),
    });
    if SIMULATOR_DATA.set(Arc::clone(&data)).is_err() {
        plugin_message("Simulator data was already initialized");
        return;
    }

    let Some(runtime) = RUNTIME_HANDLE.get() else {
        plugin_message("Runtime handle unavailable at simulation start");
        return;
    };
    if !no_info {
        let command = format!("surfer sim --address {bind_address}");
        plugin_message(format!("To connect to this simulator, run: {command}"));
    }
    runtime.spawn(message_loop(Arc::clone(&data), bind_address));

    let _cb = vpi::register_cb(CbReason::NextSimTime, next_time_step);

    plugin_message("Waiting for Surfer to connect...");
    if wait_for_run_command() {
        plugin_message("Simulation started!");
    }
}

fn next_time_step(_data: &CbData) {
    if !require_simulator_thread("next-simulation-time callback") {
        return;
    }
    if process_pending_simulator_commands() {
        return;
    }

    let sim_time_for_guard = current_sim_time();
    let prev_cb_time = LAST_NEXTSIM_CB_TIME.swap(sim_time_for_guard, Ordering::Relaxed);

    let now_ms = now_millis();
    let last_time_ms = LAST_SEND_TIME_MS.load(Ordering::Relaxed);

    if now_ms - last_time_ms >= INTERVAL_MS {
        LAST_SEND_TIME_MS.store(now_ms, Ordering::Relaxed);

        if let Some(connection) = CLIENT_CONNECTION.get() {
            connection.send(SimulatorToSurferMessage::CurrentSimulationTime {
                time: current_sim_time(),
            });
        }
    }

    // Avoid self-sustaining callback loops at a fixed simulation timestamp.
    if sim_time_for_guard != prev_cb_time {
        let _cb = vpi::register_cb(CbReason::NextSimTime, next_time_step);
    }
}

fn end_of_sim(_data: &CbData) {
    if !require_simulator_thread("end-of-simulation callback") {
        return;
    }

    SIMULATION_DONE.store(true, Ordering::SeqCst);
    SIMULATION_PAUSED.store(false, Ordering::SeqCst);
    plugin_message("end_of_sim: transitioning to Done");
    {
        let Ok(mut value_changes) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            plugin_message("Failed to lock VALUE_CHANGES mutex while ending simulation");
            return;
        };

        value_changes.ensure_time_step(SimulationTimestamp::from_ticks(current_sim_time()));
    }
    send_changed_values(SimulationStatus::Done);
    plugin_message(
        "End of simulation reached. All values sent to Surfer. Plugin will now block indefinitely.",
    );
    wait_for_finish_command();
}

#[allow(clippy::too_many_lines)]
fn handle_surfer_message(
    msg: &SurferToSimulatorMessage,
    data: &StaticSimData,
) -> SimulatorToSurferMessage {
    if !require_simulator_thread("protocol message handling") {
        return SimulatorToSurferMessage::Acknowledge {
            success: false,
            error: Some("Protocol message handling requires the simulator thread".into()),
        };
    }
    match msg {
        SurferToSimulatorMessage::RequestSimulatorInfo => data.simulator_info(),
        SurferToSimulatorMessage::RunSimulation { time: maybe_delay } => {
            plugin_message(format!("RunSimulation received: time={maybe_delay:?}"));
            if SIMULATION_DONE.load(Ordering::SeqCst) {
                SimulatorToSurferMessage::ValueChanges {
                    time_steps: [].to_vec(),
                    status: SimulationStatus::Done,
                }
            } else {
                if let Some(delay) = maybe_delay {
                    let now = SimulationTimestamp::from_ticks(current_sim_time());
                    let Some(duration) = delay.checked_delay(now, u64::MAX) else {
                        plugin_message("Run deadline is in the past or exceeds VPI time range");
                        return SimulatorToSurferMessage::Acknowledge {
                            success: false,
                            error: Some(
                                "Run deadline is in the past or exceeds VPI time range".into(),
                            ),
                        };
                    };
                    if duration.ticks() == 0 {
                        register_end_of_sim_time_callback();
                    } else {
                        register_timeout_callback(duration.ticks());
                    }
                }
                SIMULATION_PAUSED.store(false, Ordering::SeqCst);
                SimulatorToSurferMessage::Acknowledge {
                    success: true,
                    error: None,
                }
            }
        }
        SurferToSimulatorMessage::PauseSimulation => {
            register_end_of_sim_time_callback();
            SimulatorToSurferMessage::Acknowledge {
                success: true,
                error: None,
            }
        }
        SurferToSimulatorMessage::RequestHierarchy => {
            debug_message(format!(
                "sending hierarchy: {} elements, timescale exponent={}",
                data.hierarchy.len(),
                data.time_precision
            ));
            data.hierarchy_message()
        }
        SurferToSimulatorMessage::TrackVariableChanges { ids } => {
            debug_message(format!("tracking {} signal changes", ids.len()));
            for id in ids {
                set_callback_on_signal(*id, data.id_to_handle.as_ref());
            }
            SimulatorToSurferMessage::Acknowledge {
                success: true,
                error: None,
            }
        }
        SurferToSimulatorMessage::UntrackVariableChanges { ids } => {
            debug_message(format!("untracking {} signal changes", ids.len()));
            remove_callbacks(ids);
            SimulatorToSurferMessage::Acknowledge {
                success: true,
                error: None,
            }
        }
        SurferToSimulatorMessage::Shutdown => {
            plugin_message("Shutdown received from Surfer, closing plugin connection.");
            SimulatorToSurferMessage::Acknowledge {
                success: true,
                error: None,
            }
        }
    }
}

fn process_simulator_command(command: SimulatorCommand) -> CommandDisposition {
    if !require_simulator_thread("queued simulator command") {
        if let SimulatorCommand::Message { response, .. } = command {
            let _ = response.send(SimulatorToSurferMessage::Acknowledge {
                success: false,
                error: Some("Queued command requires the simulator thread".into()),
            });
        }
        return CommandDisposition::Continue;
    }

    match command {
        SimulatorCommand::Message { message, response } => {
            let result = match SIMULATOR_DATA.get() {
                Some(data) => handle_surfer_message(&message, data),
                None => SimulatorToSurferMessage::Acknowledge {
                    success: false,
                    error: Some("Simulator data unavailable".into()),
                },
            };
            let resumes_simulation = matches!(
                (&message, &result),
                (
                    SurferToSimulatorMessage::RunSimulation { .. },
                    SimulatorToSurferMessage::Acknowledge { success: true, .. }
                )
            ) && !SIMULATION_DONE.load(Ordering::SeqCst);
            let _ = response.send(result);
            if resumes_simulation {
                CommandDisposition::ResumeSimulation
            } else {
                CommandDisposition::Continue
            }
        }
        SimulatorCommand::Finish => {
            plugin_message("Finishing simulator from simulator thread");
            vpi::control(Control::Finish);
            CommandDisposition::Finish
        }
    }
}

fn receive_simulator_command() -> Option<SimulatorCommand> {
    if !require_simulator_thread("waiting for simulator command") {
        return None;
    }
    let Some(runtime) = RUNTIME_HANDLE.get() else {
        plugin_message("Runtime handle unavailable while waiting for simulator command");
        return None;
    };
    let Some(receiver) = SIMULATOR_COMMAND_RECEIVER.get() else {
        plugin_message("Simulator command receiver unavailable");
        return None;
    };
    let Ok(mut receiver) = receiver.lock() else {
        plugin_message("Failed to lock simulator command receiver");
        return None;
    };
    // Startup, pause, and end callbacks may wait here. Tokio workers continue
    // servicing the connection and can enqueue the command being awaited.
    runtime.block_on(receiver.recv())
}

// Startup and paused callbacks wait until RunSimulation is processed here.
fn wait_for_run_command() -> bool {
    loop {
        let Some(command) = receive_simulator_command() else {
            return false;
        };
        match process_simulator_command(command) {
            CommandDisposition::Continue => {}
            CommandDisposition::ResumeSimulation => return true,
            CommandDisposition::Finish => return false,
        }
    }
}

// The end-of-simulation callback remains on the simulator thread until Finish.
fn wait_for_finish_command() {
    loop {
        let Some(command) = receive_simulator_command() else {
            return;
        };
        if process_simulator_command(command) == CommandDisposition::Finish {
            return;
        }
    }
}

fn process_pending_simulator_commands() -> bool {
    let Some(receiver) = SIMULATOR_COMMAND_RECEIVER.get() else {
        return false;
    };
    // While simulation is active, next_time_step polls without blocking so
    // commands are handled on the simulator thread between simulation events.
    loop {
        let command = match receiver.lock() {
            Ok(mut receiver) => match receiver.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    return false;
                }
            },
            Err(_) => {
                plugin_message("Failed to lock simulator command receiver");
                return false;
            }
        };
        if process_simulator_command(command) == CommandDisposition::Finish {
            return true;
        }
    }
}

async fn message_loop(data: Arc<StaticSimData>, bind_address: String) {
    let Some(commands) = SIMULATOR_COMMAND_SENDER.get() else {
        plugin_message("Simulator command queue unavailable");
        return;
    };
    surfer_simulator_protocol::plugin::tcp::serve(
        &bind_address,
        data.simulator_info(),
        &CLIENT_CONNECTION,
        commands,
        plugin_message,
        debug_message,
    )
    .await;
}

pub extern "C" fn vpi_plugin_startup() {
    if SIMULATOR_THREAD_ID.set(thread::current().id()).is_err() {
        eprintln!("VPI plugin startup called more than once");
        return;
    }
    debug_message("VPI plugin startup");
    VALUE_CHANGES
        .set(Mutex::new(ValueChangeBuffer::default()))
        .ok();
    CALLBACK_STORE.set(Arc::new(CallbackStore::new())).ok();
    let (command_sender, command_receiver) = mpsc::channel(32);
    SIMULATOR_COMMAND_SENDER.set(command_sender).ok();
    SIMULATOR_COMMAND_RECEIVER
        .set(Mutex::new(command_receiver))
        .ok();

    let start_ms = now_millis();
    LAST_SEND_TIME_MS.store(start_ms, Ordering::Relaxed);

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            plugin_message(format!("Failed to create Tokio runtime: {err:?}"));
            return;
        }
    };
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();

    Box::leak(Box::new(runtime));

    let _start_cb = vpi::register_cb(CbReason::StartOfSimulation, start_of_sim);
    let _end_cb = vpi::register_cb(CbReason::EndOfSimulation, end_of_sim);
    debug_message("registered simulation lifecycle callbacks");
}

fn get_startup_argument_values() -> (String, bool) {
    let options = StartupOptions::parse(vpi::simulator_info().arguments);
    DEBUG_ENABLED.store(options.debug, Ordering::Relaxed);
    vpi::printf(format!("Binding simulator plugin to {}", options.address));
    (options.address, options.no_info)
}

/// Returns a sibling handle with the same name as the given port signal, if one exists.
fn sibling_value_handle_for_port(signal: &Handle) -> Handle {
    let Some(name) = signal.get_name() else {
        return Handle::null();
    };

    for sibling in signal.get(ObjectType::Scope).iterators(&[
        ObjectType::Net,
        ObjectType::NetArray,
        ObjectType::PackedArrayNet,
        ObjectType::Variables,
    ]) {
        if sibling.get_name().as_deref() == Some(name.as_str()) {
            return sibling;
        }
    }

    Handle::null()
}

fn callback_target_handle(signal: &Handle) -> Option<Handle> {
    if let Some(ObjectType::Port | ObjectType::PortBit) = signal.get_type() {
        let high_conn = signal.get(ObjectType::HighConn);
        if !high_conn.is_null() {
            return Some(high_conn);
        }

        let low_conn = signal.get(ObjectType::LowConn);
        if !low_conn.is_null() {
            return Some(low_conn);
        }

        let expr = signal.get(ObjectType::Expr);
        if !expr.is_null() {
            return Some(expr);
        }

        let sibling = sibling_value_handle_for_port(signal);
        if !sibling.is_null() {
            return Some(sibling);
        }
    }
    None
}

fn set_callback_on_signal(id: SimId, id_to_handle: &HashMap<SimId, SafeHandle>) {
    let Some(callback_store) = CALLBACK_STORE.get() else {
        vpi::printf(format!(
            "Callback store unavailable while tracking signal {id:?}"
        ));
        return;
    };
    if callback_store.contains_callback(id) {
        vpi::printf(format!("Callback on {id:?} already exists"));
    } else {
        let Some(signal) = id_to_handle.get(&id) else {
            vpi::printf(format!("Signal handle not found for {id:?}"));
            return;
        };

        let _ = signal.with_handle(|sig_handle| {
            let callback_target = callback_target_handle(sig_handle);
            let value_handle = callback_target.as_ref().unwrap_or(sig_handle);
            let value_type = preferred_callback_value_type(value_handle);
            debug_message(format!(
                "registering value callback: id={id:?}, signal_type={:?}, target_type={:?}, value_type={value_type:?}",
                sig_handle.get_type(),
                value_handle.get_type()
            ));

            // Some simulators expose array elements as select handles that are not reliable
            // callback registration targets.
            if matches!(
                value_handle.get_type(),
                Some(
                    ObjectType::PartSelect
                        | ObjectType::IndexedPartSelect
                        | ObjectType::VarSelect
                        | ObjectType::RegBit
                        | ObjectType::NetBit
                        | ObjectType::Bit
                )
            ) {
                debug_message(format!(
                    "recording initial value without callback for id={id:?} (target_type={:?})",
                    value_handle.get_type()
                ));
                record_value_change_with_format(value_handle, id, None, value_type);
                return;
            }

            let callback_handle = value_handle.register_value_change_cb(value_type, move |data| {
                let cb_time = data.time.as_ref().and_then(Time::to_u64);
                record_value_change_from_cbdata(data, id, cb_time, value_type);
            });
            callback_store.insert_callback(id, SafeHandle::from_handle(callback_handle));
            record_value_change_with_format(value_handle, id, None, value_type);
        });
    }
}

fn remove_callbacks(sim_ids: &[SimId]) {
    let Some(callback_store) = CALLBACK_STORE.get() else {
        vpi::printf("Callback store unavailable while untracking signals");
        return;
    };
    let Ok(mut time_steps) = VALUE_CHANGES
        .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
        .lock()
    else {
        vpi::printf("Failed to lock VALUE_CHANGES mutex while untracking signals");
        return;
    };

    let sim_ids: HashSet<SimId> = sim_ids.iter().copied().collect();

    for &sim_id in &sim_ids {
        if let Some(callback_handle) = callback_store.remove_callback(sim_id) {
            let _ = callback_handle.with_handle(vpi::remove_cb);
        }
    }

    time_steps.remove_signals(&sim_ids);
}

fn record_value_change_with_format(
    signal_handle: &Handle,
    id: SimId,
    cb_time: Option<u64>,
    value_type: ValueType,
) {
    let Ok(mut global_changes) = VALUE_CHANGES
        .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
        .lock()
    else {
        vpi::printf("Failed to lock VALUE_CHANGES mutex while recording a value change");
        return;
    };

    if let Some(value) = vpi_signal_value_to_vcd_bytes_with_type(signal_handle, value_type) {
        let current_time = sim_time_from_callback(cb_time);

        global_changes.record(
            SimulationTimestamp::from_ticks(current_time),
            id,
            SignalValue::VCDValue(value),
        );
    }
}

fn record_value_change_from_cbdata(
    data: &CbData,
    id: SimId,
    cb_time: Option<u64>,
    fallback_value_type: ValueType,
) {
    if let Some(value) = data.value.as_ref()
        && let Some(vcd_bytes) = vpi_value_to_vcd_bytes(value, &data.obj)
    {
        let Ok(mut global_changes) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            vpi::printf("Failed to lock VALUE_CHANGES mutex from value-change callback");
            return;
        };

        let current_time = sim_time_from_callback(cb_time);
        global_changes.record(
            SimulationTimestamp::from_ticks(current_time),
            id,
            SignalValue::VCDValue(vcd_bytes),
        );
        return;
    }

    record_value_change_with_format(&data.obj, id, cb_time, fallback_value_type);
}

fn vpi_signal_value_to_vcd_bytes_with_type(sig: &Handle, value_type: ValueType) -> Option<Vec<u8>> {
    let value = sig.get_value(value_type)?;
    vpi_value_to_vcd_bytes(&value, sig)
}

fn vpi_value_to_vcd_bytes(value: &Value, sig: &Handle) -> Option<Vec<u8>> {
    let declared_bits = sig.get_size().unwrap_or(1).clamp(1, 64) as usize;
    let (prefix, mut payload) = match value {
        Value::BinStr(s) if s.is_empty() => return None,
        Value::BinStr(s) => (b'b', s.clone()),
        Value::OctStr(s) => {
            // Convert octal string to binary string
            let binary_string: String = s
                .chars()
                .filter_map(|c| c.to_digit(8))
                .map(|digit| format!("{digit:03b}"))
                .collect();
            (b'b', binary_string)
        }
        Value::HexStr(s) => {
            // Convert hex string to binary string
            let binary_string: String = s
                .chars()
                .filter_map(|c| c.to_digit(16))
                .map(|digit| format!("{digit:04b}"))
                .collect();
            (b'b', binary_string)
        }
        Value::DecStr(s) => {
            // Convert decimal string to binary string
            let decimal_value = s.parse::<u64>().ok()?;
            let binary_string = uint_to_bit_string(decimal_value, declared_bits);
            (b'b', binary_string)
        }
        Value::String(s) => (b's', s.clone()),
        Value::Real(f) => (b'r', f.to_string()),
        Value::ShortReal(f) => (b'r', f.to_string()),
        Value::Time(Time::Sim(t)) => (b'b', uint_to_bit_string(*t, declared_bits)),
        Value::Time(Time::ScaledReal(t)) => (b'r', t.to_string()),
        Value::Int(n) => (b'b', int_to_bit_string(*n, declared_bits)),
        Value::ShortInt(n) => (b'b', int_to_bit_string(*n, declared_bits)),
        Value::LongInt(n) => (b'b', int_to_bit_string(*n, declared_bits)),
        Value::Scalar(scalar) => return Some(vec![char::from(*scalar) as u8]),
        Value::Vector(bits) if bits.is_empty() => return None,
        Value::Vector(bits) => return Some(bits.as_vcd_value()),
        Value::Strength(_) | Value::ObjType(_) | Value::Suppress | Value::Time(Time::Suppress) => {
            return None;
        }
    };

    if matches!(value, Value::String(_))
        && let Some(stripped_prefix) = payload.strip_prefix('"')
        && let Some(stripped) = stripped_prefix.strip_suffix('"')
    {
        payload = stripped.to_string();
    }
    if prefix == b'b' {
        let p = payload.trim_start_matches('0');
        if p.is_empty() {
            payload = "0".to_string();
        } else {
            payload = p.to_string();
        }
    }
    let mut buf = vec![prefix];
    buf.extend_from_slice(payload.as_bytes());
    Some(buf)
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_message_works_without_a_simulator() {
        super::plugin_message("VPI unit-test diagnostic");
    }

    #[test]
    fn explicit_aliases_resolve_transitively_and_without_cycles() {
        let mut ids = super::IdAliases::default();
        ids.merge_ids(7, 3);
        ids.merge_ids(3, 1);
        ids.merge_ids(1, 7);
        assert_eq!(ids.resolve_id(7), 1);
        assert_eq!(ids.resolve_id(3), 1);
        assert_eq!(ids.resolve_id(1), 1);
        assert_eq!(ids.resolve_id(8), 8);
        assert!(ids.is_alias(7));
        assert!(ids.is_alias(3));
        assert!(!ids.is_alias(1));
        assert!(!ids.is_alias(8));
    }
}
