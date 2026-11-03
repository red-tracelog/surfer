use portable_atomic::AtomicU128;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Instant;
use surfer_simulator_protocol::plugin::{
    ClientConnection, SimulatorCommand, SimulatorData, StartupOptions, ValueChangeBuffer,
    uint_to_bit_string,
};
use surfer_simulator_protocol::{
    HierarchyElement, ScopeType, SignalEncoding, SignalValue, SimId, SimulationStatus,
    SimulationTimestamp, SimulatorToSurferMessage, SurferToSimulatorMessage, VarDirection,
    VarIndex, VarType,
};
use tokio::sync::mpsc;
use vhpi::{
    CbData, CbReason, ClassKind, Control, Format, Handle, IntProperty, Mode, OneToMany, OneToOne,
    StrProperty, Time, Value, printf, register_cb,
};

// The startup/callback thread owns VHPI. Tokio workers handle TCP and send
// simulator-bound requests through this command queue.
static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static SIMULATOR_THREAD_ID: OnceLock<thread::ThreadId> = OnceLock::new();
static SIMULATOR_COMMAND_SENDER: OnceLock<mpsc::Sender<SimulatorCommand>> = OnceLock::new();
static SIMULATOR_COMMAND_RECEIVER: OnceLock<Mutex<mpsc::Receiver<SimulatorCommand>>> =
    OnceLock::new();
static SIMULATOR_DATA: OnceLock<Arc<StaticSimData>> = OnceLock::new();
static VALUE_CHANGES: OnceLock<Mutex<ValueChangeBuffer>> = OnceLock::new();
static TIMEOUT_CALLBACK: OnceLock<Mutex<Option<SafeHandle>>> = OnceLock::new();
static TIMEOUT_GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_SEND_TIME_MS: AtomicU128 = AtomicU128::new(0);
static CALLBACK_STORE: OnceLock<Arc<CallbackStore>> = OnceLock::new();
static SIMULATION_DONE: AtomicBool = AtomicBool::new(false);
static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

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
        eprintln!("Refusing VHPI operation outside simulator thread: {operation}");
        false
    }
}

fn plugin_message(message: impl AsRef<str>) {
    // Unit tests run without a simulator to provide the Linux vhpi_printf symbol.
    #[cfg(not(test))]
    if is_simulator_thread() {
        printf!("{}", message.as_ref());
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

/// The single active Surfer client connection.
static CLIENT_CONNECTION: OnceLock<ClientConnection> = OnceLock::new();

fn now_millis() -> u128 {
    // Measure update intervals independently of changes to the system clock.
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis()
}

fn protocol_time_from_vhpi(time: vhpi::Time) -> Option<u64> {
    match SimulationTimestamp::try_from(time.to_i64()) {
        Ok(time) => Some(time.ticks()),
        Err(_) => {
            plugin_message("Rejecting negative VHPI simulation timestamp");
            None
        }
    }
}

type IdToHandleMap = HashMap<SimId, SafeHandle>;

type StaticSimData = SimulatorData<IdToHandleMap>;

struct SendHandle(Handle);

// `Handle` is not Send. This wrapper allows shared storage, while SafeHandle
// restricts operations on it to the thread that created it.
unsafe impl Send for SendHandle {}

pub struct SafeHandle {
    owner_thread: thread::ThreadId,
    handle: Mutex<SendHandle>,
}

impl SafeHandle {
    fn new(handle: Handle) -> Self {
        Self {
            owner_thread: SIMULATOR_THREAD_ID
                .get()
                .copied()
                .unwrap_or_else(|| thread::current().id()),
            handle: Mutex::new(SendHandle(handle)),
        }
    }

    fn with_handle<Output>(&self, operation: impl FnOnce(&Handle) -> Output) -> Option<Output> {
        if self.owner_thread != thread::current().id() {
            plugin_message("Refusing VHPI handle access from non-owner thread");
            return None;
        }
        let Ok(handle) = self.handle.lock() else {
            plugin_message("Failed to lock VHPI handle");
            return None;
        };
        Some(operation(&handle.0))
    }
}

type CallbackStore = surfer_simulator_protocol::plugin::CallbackStore<SafeHandle>;

/// Only accept readable whole-signal representatives. Never replace an indexed
/// or selected name with its parent: that would merge distinct slices/elements.
fn canonical_signal_handle(handle: Handle) -> Handle {
    if matches!(
        handle.get_kind(),
        Some(ClassKind::SigDecl | ClassKind::PortDecl)
    ) {
        let simulated = handle.handle(OneToOne::SimNet);
        if !simulated.is_null()
            && matches!(
                simulated.get_kind(),
                Some(ClassKind::SigDecl | ClassKind::PortDecl)
            )
            && simulated.get(IntProperty::Size) == handle.get(IntProperty::Size)
        {
            return simulated;
        }
    }
    handle
}

fn whole_signal_alias_target(mut target: Handle) -> Option<Handle> {
    let mut seen = Vec::new();
    while !target.is_null() && target.get_kind() == Some(ClassKind::AliasDecl) {
        if seen.iter().any(|previous| previous == &target) {
            return None;
        }
        let next = target.handle(OneToOne::AliasedName);
        seen.push(target);
        target = next;
    }
    (!target.is_null()
        && matches!(
            target.get_kind(),
            Some(ClassKind::SigDecl | ClassKind::PortDecl)
        ))
    .then_some(target)
}

/// The protocol will use ID's to refer to handles so we need some way to keep track of them
pub struct IdStore {
    next_id: AtomicU32,
    // Canonical full names only narrow the search; handle comparison establishes identity.
    identities: HashMap<(Option<String>, SignalEncoding), Vec<SimId>>,
    pub id_to_handle: IdToHandleMap,
}

impl IdStore {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(0),
            identities: HashMap::new(),
            id_to_handle: HashMap::new(),
        }
    }

    pub fn create_id(&mut self, handle: Handle, encoding: SignalEncoding) -> SimId {
        let handle = canonical_signal_handle(handle);
        let key = (handle.get_full_name(), encoding);
        if let Some(candidates) = self.identities.get(&key) {
            for id in candidates {
                if self.id_to_handle.get(id).is_some_and(|existing| {
                    existing.with_handle(|existing| existing == &handle) == Some(true)
                }) {
                    return *id;
                }
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        self.id_to_handle.insert(id, SafeHandle::new(handle));
        self.identities.entry(key).or_default().push(id);
        id
    }
}

/// Get the hierarchy so that it can be sent back to Surfer
fn collect_hierarchy_elements_from_region(
    region: &Handle,
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    scope_type: ScopeType,
) {
    let region_name = region.get_name().unwrap_or_default();
    let element_count = h.len();
    debug_message(format!(
        "hierarchy: entering region {region_name:?} ({scope_type:?}), elements_before={element_count}"
    ));
    h.push(HierarchyElement::PushScope {
        name: region_name.clone(),
        scope_type,
    });

    let mut port_count = 0;
    for port in region.iterator(OneToMany::PortDecls) {
        port_count += 1;
        debug_message(format!(
            "hierarchy: scanning port {:?}, kind={:?}",
            port.get_name().unwrap_or_default(),
            port.handle(OneToOne::Type).get_kind()
        ));
        let type_handle = port.handle(OneToOne::Type);
        let name = port.get_name().unwrap_or_default();
        let var_direction = port.get_mode().map(|dir| match dir {
            Mode::In => VarDirection::Input,
            Mode::Out => VarDirection::Output,
            Mode::Inout => VarDirection::InOut,
            Mode::Buffer => VarDirection::Buffer,
            Mode::Linkage => VarDirection::Linkage,
        });

        add_var(
            h,
            id_store,
            port,
            &type_handle,
            name,
            Vec::new(),
            None,
            var_direction,
            None,
        );
    }

    /* TODO: Handle parameters correctly
    for parameter in region.iterator(OneToMany::GenericDecls) {
        let type_handle = parameter.handle(OneToOne::Type);
        let name = parameter.get_name().unwrap_or_default();

        add_var(
            h,
            id_store,
            parameter,
            &type_handle,
            name,
            Vec::new(),
            None,
            None,
            None,
        );
    }
    */

    let mut signal_count = 0;
    for sig in region.iterator(OneToMany::SigDecls) {
        signal_count += 1;
        let sig_name = sig.get_name().unwrap_or_default();
        let type_handle = sig.handle(OneToOne::Type);
        debug_message(format!(
            "hierarchy: scanning signal {:?}, type_kind={:?}",
            sig_name,
            type_handle.get_kind()
        ));
        let name = sig_name;

        add_var(
            h,
            id_store,
            sig,
            &type_handle,
            name,
            Vec::new(),
            None,
            None,
            None,
        );
    }

    // A whole-object alias adds a hierarchy name, not another value stream.
    // Slices and indexed/selected names need explicit mapping and are not folded
    // into their parent signal here.
    for alias in region.iterator(OneToMany::AliasDecls) {
        let Some(target) = whole_signal_alias_target(alias.handle(OneToOne::AliasedName)) else {
            continue;
        };
        let alias_type = alias.handle(OneToOne::Type);
        let type_handle = if alias_type.is_null() {
            target.handle(OneToOne::Type)
        } else {
            alias_type
        };
        add_var(
            h,
            id_store,
            target,
            &type_handle,
            alias.get_name().unwrap_or_default(),
            Vec::new(),
            None,
            None,
            None,
        );
    }

    let mut child_region_count = 0;
    for sub in region.iterator(OneToMany::InternalRegions) {
        child_region_count += 1;
        debug_message(format!(
            "hierarchy: descending into child region {:?}",
            sub.get_name().unwrap_or_default()
        ));
        collect_hierarchy_elements_from_region(&sub, h, id_store, ScopeType::Module);
    }

    h.push(HierarchyElement::PopScope);
    debug_message(format!(
        "hierarchy: leaving region {:?}, ports={port_count}, signals={signal_count}, child_regions={child_region_count}, elements_added={}",
        region_name,
        h.len().saturating_sub(element_count)
    ));
}

#[allow(clippy::too_many_arguments)]
fn add_array_element(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    elem: Handle,
    elem_type_handle: &Handle,
    indices: Vec<String>,
    needs_full_recursion: bool,
    cached_parse_type: &Option<ParseType>,
    cached_enum_map: &Option<Vec<(String, String)>>,
    simple_elem_meta: &Option<(String, Option<VarIndex>)>,
    var_direction: Option<VarDirection>,
) {
    let elem_name = array_index_name(&indices);
    if needs_full_recursion {
        add_var(
            h,
            id_store,
            elem,
            elem_type_handle,
            elem_name,
            indices,
            None,
            var_direction,
            None,
        );
    } else if let (
        Some(ParseType::Simple(var_type, signal_encoding)),
        Some((type_name, var_range)),
    ) = (cached_parse_type, simple_elem_meta)
    {
        let id = id_store.create_id(elem, *signal_encoding);
        h.push(HierarchyElement::AddVar {
            id,
            name: elem_name,
            type_name: type_name.clone(),
            var_type: *var_type,
            signal_encoding: *signal_encoding,
            var_direction,
            enum_map: (*cached_enum_map).clone(),
            index: *var_range,
            is_parameter: false,
        });
    } else {
        add_var(
            h,
            id_store,
            elem,
            elem_type_handle,
            elem_name,
            indices,
            (*cached_enum_map).clone(),
            var_direction,
            (*cached_parse_type).clone(),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn add_var(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    sig: Handle,
    type_handle: &Handle,
    name: String,
    array_indices: Vec<String>,
    mut enum_map: Option<Vec<(String, String)>>,
    var_direction: Option<VarDirection>,
    cached_parse_type: Option<ParseType>,
) {
    let type_kind = type_handle.get_kind();
    debug_message(format!(
        "hierarchy: classifying {:?}, type_kind={type_kind:?}, name={name:?}, array_indices={array_indices:?}, direction={var_direction:?}",
        sig.get_name().unwrap_or_default()
    ));

    if type_kind.is_none() {
        printf!(
            "signal {} with unsupported kind",
            sig.get_name().unwrap_or_else(|| "<unknown>".to_string())
        );
        return;
    }

    if enum_map.is_none() && matches!(type_kind, Some(ClassKind::EnumTypeDecl)) {
        enum_map = type_handle.enum_literals().map(|entries| {
            entries
                .into_iter()
                .enumerate()
                .map(|(ordinal, value)| (ordinal.to_string(), value))
                .collect()
        });
    }

    let parse_type = cached_parse_type.unwrap_or_else(|| {
        if let Some(composite) = composite_parse_type(type_handle) {
            return composite;
        }

        let value = sig.get_value(Format::ObjType);
        match value {
            Ok(value) => get_wellen_type(&value, type_handle),
            Err(err) => {
                // Some backends cannot read arrays in ObjType format. Expand
                // those without warning, including constrained array subtypes.
                if resolved_type_kind(type_handle) != Some(ClassKind::ArrayTypeDecl) {
                    printf!(
                        "Failed to get value of {:?} (kind: {:?}, err: {:?})",
                        sig.get_name().unwrap_or_else(|| "<unknown>".to_string()),
                        type_kind,
                        err
                    );
                }
                ParseType::ArrayType
            }
        }
    });
    debug_message(format!(
        "hierarchy: expanding {:?}, parse_type={parse_type:?}, name={name:?}, array_indices={array_indices:?}, direction={var_direction:?}",
        sig.get_name().unwrap_or_default()
    ));
    match parse_type {
        ParseType::RecordType => {
            add_record(h, id_store, sig, name, array_indices, var_direction);
        }
        ParseType::ArrayType => add_array(
            h,
            id_store,
            sig,
            type_handle,
            name,
            array_indices,
            var_direction,
        ),
        ParseType::Simple(var_type, signal_encoding) => {
            let index = var_index(type_handle, &var_type, &signal_encoding);

            let id = id_store.create_id(sig, signal_encoding);

            let name = if array_indices.is_empty() {
                name
            } else {
                array_index_name(&array_indices)
            };

            debug_message(format!(
                "hierarchy: added signal id={id:?}, name={name:?}, type_name={:?}, var_type={var_type:?}, encoding={signal_encoding:?}, index={index:?}",
                type_handle.get_name().unwrap_or_default()
            ));
            h.push(HierarchyElement::AddVar {
                id,
                name,
                type_name: type_handle.get_name().unwrap_or_default(),
                var_type,
                signal_encoding,
                var_direction,
                enum_map,
                index,
                is_parameter: false,
            });
        }
        ParseType::Unhandled => todo!(),
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn add_array(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    sig: Handle,
    type_handle: &Handle,
    name: String,
    array_indices: Vec<String>,
    var_direction: Option<VarDirection>,
) {
    let is_nested_array = !array_indices.is_empty();
    let arr_name = name;
    let elem_type_handle = type_handle.handle(OneToOne::ElemType);
    if !is_nested_array {
        h.push(HierarchyElement::PushScope {
            name: arr_name.clone(),
            scope_type: ScopeType::VhdlArray,
        });
    }

    let num_dims = type_handle.get(IntProperty::NumDimensions);
    debug_message(format!(
        "hierarchy: array {arr_name:?}, dimensions={num_dims}, element_kind={:?}",
        elem_type_handle.get_kind()
    ));

    let elem_kind = resolved_type_kind(&elem_type_handle);
    let needs_full_recursion = matches!(
        elem_kind,
        Some(ClassKind::ArrayTypeDecl | ClassKind::RecordTypeDecl) | None
    );
    let mut elem_iter = sig.iterator(OneToMany::IndexedNames).peekable();

    let (elem_cached_parse_type, elem_cached_enum_map) = if needs_full_recursion {
        (None, None)
    } else {
        let elem_enum_map = if matches!(elem_kind, Some(ClassKind::EnumTypeDecl)) {
            elem_type_handle.enum_literals().map(|entries| {
                entries
                    .into_iter()
                    .enumerate()
                    .map(|(ordinal, value)| (ordinal.to_string(), value))
                    .collect()
            })
        } else {
            None
        };
        let elem_parse_type = elem_iter
            .peek()
            .and_then(|elem| elem.get_value(Format::ObjType).ok())
            .map(|value| get_wellen_type(&value, &elem_type_handle));
        (elem_parse_type, elem_enum_map)
    };

    let simple_elem_meta: Option<(String, Option<VarIndex>)> =
        match (&elem_cached_parse_type, needs_full_recursion) {
            (Some(ParseType::Simple(var_type, signal_encoding)), false) => {
                let type_name = elem_type_handle.get_name().unwrap_or_default();
                let var_range = var_index(&elem_type_handle, var_type, signal_encoding);
                Some((type_name, var_range))
            }
            _ => None,
        };

    if num_dims > 1 {
        let dims = collect_dim_infos(type_handle);
        debug_message(format!(
            "hierarchy: array {arr_name:?} has dimension ranges {dims:?}"
        ));
        for (linear_idx, elem) in elem_iter.enumerate() {
            let dimension_indices = linear_to_nd_indices(linear_idx, &dims);
            let mut indices = array_indices.clone();
            indices.extend(dimension_indices.iter().map(ToString::to_string));
            debug_message(format!(
                "hierarchy: array {arr_name:?} element linear_index={linear_idx}, indices={dimension_indices:?}"
            ));
            add_array_element(
                h,
                id_store,
                elem,
                &elem_type_handle,
                indices,
                needs_full_recursion,
                &elem_cached_parse_type,
                &elem_cached_enum_map,
                &simple_elem_meta,
                var_direction,
            );
        }
    } else {
        for (index, elem) in type_handle.index_range().zip(&mut elem_iter) {
            let mut indices = array_indices.clone();
            indices.push(index.to_string());
            debug_message(format!(
                "hierarchy: array {arr_name:?} element index={index}"
            ));
            add_array_element(
                h,
                id_store,
                elem,
                &elem_type_handle,
                indices,
                needs_full_recursion,
                &elem_cached_parse_type,
                &elem_cached_enum_map,
                &simple_elem_meta,
                var_direction,
            );
        }
    }

    if !is_nested_array {
        h.push(HierarchyElement::PopScope);
    }
}

fn var_index(
    elem_type_handle: &Handle,
    var_type: &VarType,
    signal_encoding: &SignalEncoding,
) -> Option<VarIndex> {
    match (var_type, signal_encoding) {
        (
            VarType::StdLogicVector | VarType::StdULogicVector | VarType::BitVector,
            SignalEncoding::BitVector(_),
        ) => get_var_index(elem_type_handle),
        _ => None,
    }
}

fn add_record(
    h: &mut Vec<HierarchyElement>,
    id_store: &mut IdStore,
    sig: Handle,
    name: String,
    array_indices: Vec<String>,
    var_direction: Option<VarDirection>,
) {
    let record_name = if array_indices.is_empty() {
        name
    } else {
        array_index_name(&array_indices)
    };
    h.push(HierarchyElement::PushScope {
        name: record_name.clone(),
        scope_type: ScopeType::VhdlRecord,
    });

    let mut field_count = 0;
    for field in sig.iterator(OneToMany::SelectedNames) {
        field_count += 1;
        let including_parent_name = field.get_name().unwrap_or_default();
        let Some((_, field_name)) = including_parent_name.rsplit_once('.') else {
            printf!(
                "Ignoring record field with invalid name {:?}",
                including_parent_name
            );
            continue;
        };

        let field_type_handle = field.handle(OneToOne::Type);
        add_var(
            h,
            id_store,
            field,
            &field_type_handle,
            field_name.to_string(),
            Vec::new(),
            None,
            var_direction,
            None,
        );
    }

    h.push(HierarchyElement::PopScope);
    debug_message(format!(
        "hierarchy: expanded record {record_name:?} with {field_count} fields"
    ));
}

fn array_index_name(indices: &[String]) -> String {
    format!("[{}]", indices.join(", "))
}

#[derive(Debug, Clone)]
enum ParseType {
    Simple(VarType, SignalEncoding),
    ArrayType,
    RecordType,
    Unhandled,
}

/// A constrained array/record is often reported as SubtypeDecl. Inspect its
/// base declaration to classify it, while retaining the original constrained
/// handle for bounds, element types, and display metadata.
fn resolved_type_kind(type_handle: &Handle) -> Option<ClassKind> {
    if type_handle.is_null() {
        return None;
    }
    let kind = type_handle.get_kind();
    if kind != Some(ClassKind::SubtypeDecl) {
        return kind;
    }
    let base = type_handle.handle(OneToOne::BaseType);
    if base.is_null() || base == *type_handle {
        return kind;
    }
    resolved_type_kind(&base)
}

/// These composites require hierarchy expansion regardless of their values.
/// Flat vectors and strings still use ObjType to preserve native encodings.
fn composite_parse_type(type_handle: &Handle) -> Option<ParseType> {
    match resolved_type_kind(type_handle) {
        Some(ClassKind::RecordTypeDecl) => Some(ParseType::RecordType),
        Some(ClassKind::ArrayTypeDecl) => {
            if type_handle.get(IntProperty::NumDimensions) > 1 {
                return Some(ParseType::ArrayType);
            }
            let elem = type_handle.handle(OneToOne::ElemType);
            matches!(
                resolved_type_kind(&elem),
                Some(ClassKind::ArrayTypeDecl | ClassKind::RecordTypeDecl)
            )
            .then_some(ParseType::ArrayType)
        }
        _ => None,
    }
}

/// Gets the matching wellen type info for a VHPI value
/// Note that vector types other than `LogicVec` should be split into composite signals
fn get_wellen_type(value: &Value, type_handle: &Handle) -> ParseType {
    // This is split into 3 cases:
    //  1. A value can be directly translated to a wellen type
    //  2. A value needs to be split into component parts, i.e. IntVec, we will need to handle children as their own scope recursively
    //  3. Unhandled, for stuff that we have no idea what to do with need to prevent infinite recursion

    match value {
        Value::Str(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::Char(_) => ParseType::Simple(VarType::Byte, SignalEncoding::String), // TODO: Is VarType::Byte correct?
        Value::Real(_) => ParseType::Simple(VarType::Real, SignalEncoding::Real),
        Value::Logic(_) => ParseType::Simple(
            logic_var_type_from_handle(type_handle, false).unwrap_or(VarType::StdLogic),
            SignalEncoding::BitVector(1),
        ),
        // Logic vec is handled natively
        Value::LogicVec(vals) => ParseType::Simple(
            logic_var_type_from_handle(type_handle, true).unwrap_or(VarType::StdLogicVector),
            SignalEncoding::BitVector(vals.len() as u32),
        ),
        Value::BinStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::OctStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::HexStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::DecStr(_) => ParseType::Simple(VarType::String, SignalEncoding::String),
        Value::Int(_) => ParseType::Simple(VarType::Integer, SignalEncoding::BitVector(32)),
        Value::LongInt(_) => ParseType::Simple(VarType::LongInt, SignalEncoding::BitVector(64)),

        // We send the enums as strings by doing a lookup on their value
        Value::Enum(_) => ParseType::Simple(VarType::Enum, SignalEncoding::String),
        Value::SmallEnum(_) => ParseType::Simple(VarType::Enum, SignalEncoding::String),

        // VarType::Time might be a better fit (also for physical?), but I'd rather that Surfer displays the value like an integer so that its human readable
        Value::Time(_) => ParseType::Simple(VarType::LongInt, SignalEncoding::BitVector(64)),
        Value::Physical(_) => ParseType::Simple(VarType::LongInt, SignalEncoding::BitVector(64)),
        Value::SmallPhysical(_) => {
            ParseType::Simple(VarType::Integer, SignalEncoding::BitVector(32))
        }

        Value::IntVec(_) => ParseType::ArrayType,
        Value::EnumVec(_) => ParseType::ArrayType,
        Value::RealVec(_) => ParseType::ArrayType,
        Value::TimeVec(_) => ParseType::ArrayType,
        Value::LongIntVec(_) => ParseType::ArrayType,
        Value::PhysicalVec(_) => ParseType::ArrayType,
        Value::SmallEnumVec(_) => ParseType::ArrayType,
        Value::SmallPhysicalVec(_) => ParseType::ArrayType,

        Value::Unknown => ParseType::Unhandled,
    }
}

fn logic_var_type_from_handle(type_handle: &Handle, is_vector: bool) -> Option<VarType> {
    if type_handle.is_null() {
        return None;
    }

    if let Some(name) = type_handle.get_name()
        && let Some(var_type) = logic_var_type_from_name(&name, is_vector)
    {
        return Some(var_type);
    }

    if is_vector {
        let elem_type = type_handle.handle(OneToOne::ElemType);
        if !elem_type.is_null()
            && let Some(var_type) = logic_var_type_from_handle(&elem_type, false)
        {
            return logic_vector_type_from_scalar(var_type);
        }
    }

    let base_type = type_handle.handle(OneToOne::BaseType);
    if base_type.is_null() || base_type == *type_handle {
        return None;
    }
    logic_var_type_from_handle(&base_type, is_vector)
}

fn logic_var_type_from_name(type_name: &str, is_vector: bool) -> Option<VarType> {
    if is_vector {
        match type_name.to_ascii_lowercase().as_str() {
            "std_logic_vector" => Some(VarType::StdLogicVector),
            "std_ulogic_vector" => Some(VarType::StdULogicVector),
            "bit_vector" => Some(VarType::BitVector),
            _ => None,
        }
    } else {
        match type_name.to_ascii_lowercase().as_str() {
            "std_logic" => Some(VarType::StdLogic),
            "std_ulogic" => Some(VarType::StdULogic),
            "bit" => Some(VarType::Bit),
            _ => None,
        }
    }
}

fn logic_vector_type_from_scalar(var_type: VarType) -> Option<VarType> {
    match var_type {
        VarType::StdLogic => Some(VarType::StdLogicVector),
        VarType::StdULogic => Some(VarType::StdULogicVector),
        VarType::Bit => Some(VarType::BitVector),
        _ => None,
    }
}

fn get_var_index(type_handle: &Handle) -> Option<VarIndex> {
    let constraint = type_handle.iterator(OneToMany::Constraints).next()?;
    Some(VarIndex {
        msb: i64::from(constraint.get(IntProperty::LeftBound)),
        lsb: i64::from(constraint.get(IntProperty::RightBound)),
    })
}

/// One dimension of a multi-dimensional VHDL array: left bound, element count, direction.
#[derive(Debug)]
struct DimInfo {
    left: i32,
    size: usize,
    is_up: bool,
}

impl DimInfo {
    fn index_at(&self, pos: usize) -> i32 {
        if self.is_up {
            self.left + pos as i32
        } else {
            self.left - pos as i32
        }
    }
}

/// Collect one `DimInfo` per dimension by iterating the type's constraint handles.
fn collect_dim_infos(type_handle: &Handle) -> Vec<DimInfo> {
    type_handle
        .iterator(OneToMany::Constraints)
        .map(|c| {
            let left = c.get(IntProperty::LeftBound);
            let right = c.get(IntProperty::RightBound);
            let is_up = c.get(IntProperty::IsUp) != 0;
            let size = if is_up {
                (right - left + 1).max(0) as usize
            } else {
                (left - right + 1).max(0) as usize
            };
            DimInfo { left, size, is_up }
        })
        .collect()
}

/// Convert a row-major linear index into per-dimension VHDL indices.
fn linear_to_nd_indices(mut linear: usize, dims: &[DimInfo]) -> Vec<i32> {
    let mut result = vec![0i32; dims.len()];
    for i in (0..dims.len()).rev() {
        if dims[i].size > 0 {
            result[i] = dims[i].index_at(linear % dims[i].size);
            linear /= dims[i].size;
        }
    }
    result
}

fn cancel_timeout_callback() {
    TIMEOUT_GENERATION.fetch_add(1, Ordering::SeqCst);

    let Ok(mut active_callback) = TIMEOUT_CALLBACK.get_or_init(|| Mutex::new(None)).lock() else {
        printf!("Failed to lock timeout callback state");
        return;
    };

    let Some(callback_handle) = active_callback.take() else {
        return;
    };

    let _ = callback_handle.with_handle(|handle| {
        if let Err(err) = vhpi::remove_cb(handle) {
            plugin_message(format!("Failed to remove timeout callback: {err:?}"));
        }
    });
}

fn register_timeout_callback(delay: Time) -> bool {
    cancel_timeout_callback();
    let generation = TIMEOUT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    match vhpi::register_cb_after_delay(delay, move |data| timeout_callback(data, generation)) {
        Ok(callback_handle) => {
            let Ok(mut active_callback) = TIMEOUT_CALLBACK.get_or_init(|| Mutex::new(None)).lock()
            else {
                printf!("Failed to lock timeout callback state after registration");
                let _ = vhpi::remove_cb(&callback_handle);
                return false;
            };
            *active_callback = Some(SafeHandle::new(callback_handle));
            true
        }
        Err(err) => {
            printf!("Failed to register timeout callback: {:?}", err);
            false
        }
    }
}

fn register_end_of_time_step_callback() -> bool {
    cancel_timeout_callback();
    let generation = TIMEOUT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    match register_cb(CbReason::EndOfTimeStep, move |data| {
        timeout_callback(data, generation);
    }) {
        Ok(callback_handle) => {
            let Ok(mut active_callback) = TIMEOUT_CALLBACK.get_or_init(|| Mutex::new(None)).lock()
            else {
                printf!("Failed to lock timeout callback state after registration");
                let _ = vhpi::remove_cb(&callback_handle);
                return false;
            };
            *active_callback = Some(SafeHandle::new(callback_handle));
            true
        }
        Err(err) => {
            printf!("Failed to register end-of-time-step callback: {:?}", err);
            false
        }
    }
}

fn timeout_callback(_data: &CbData, generation: u64) {
    if !require_simulator_thread("timeout callback") {
        return;
    }
    if TIMEOUT_GENERATION.load(Ordering::SeqCst) != generation {
        return;
    }

    // Both delayed and end-of-time-step callbacks must be one-shot. Removing the
    // handle here prevents an end-of-time-step callback from firing again while
    // this invocation is blocked waiting for the next run.
    let callback_handle = TIMEOUT_CALLBACK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|mut active_callback| active_callback.take());
    if let Some(callback_handle) = callback_handle {
        let _ = callback_handle.with_handle(|handle| {
            if let Err(err) = vhpi::remove_cb(handle) {
                plugin_message(format!("Failed to remove fired timeout callback: {err:?}"));
            }
        });
    }

    // This is called when we want to end the simulation early, either when we pause or our cb_after_delay has been called
    plugin_message("Timeout reached. Sending values and pausing...");
    {
        let Ok(mut value_changes) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            plugin_message("Failed to lock VALUE_CHANGES mutex");
            return;
        };

        // We need some way to inform Surfer when the simulation is done
        if let Some(timestamp) = protocol_time_from_vhpi(vhpi::get_time()) {
            value_changes.ensure_time_step(SimulationTimestamp::from_ticks(timestamp));
        }
    }
    send_changed_values(SimulationStatus::Paused);

    if wait_for_run_command() {
        plugin_message("Simulation resumed!");
    }
}

fn send_changed_values(status: SimulationStatus) {
    let value_changes = {
        let Ok(mut global_changes) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            plugin_message("Failed to lock VALUE_CHANGES mutex");
            return;
        };

        global_changes.take()
    };

    if let Some(connection) = CLIENT_CONNECTION.get() {
        connection.send_value_changes(value_changes, status);
    }
}

/// Callback for the start of simulation, collect the hierarchy and open TCP-connection
fn start_of_sim(_data: &CbData) {
    if !require_simulator_thread("start-of-simulation callback") {
        return;
    }
    debug_message("start of simulation entered");
    let (bind_address, no_info) = get_startup_argument_values();

    let root = vhpi::handle(OneToOne::RootInst);

    // We read the hierarchy here so that it can be sent back
    let mut hierarchy = Vec::new();
    let mut id_store = IdStore::new();
    collect_hierarchy_elements_from_region(&root, &mut hierarchy, &mut id_store, ScopeType::Module);
    debug_message(format!(
        "hierarchy collection complete: {} elements, {} signal handles",
        hierarchy.len(),
        id_store.id_to_handle.len()
    ));

    let resolution_fs = vhpi::simulator_time_resolution().to_i64();
    let resolution_fs = if resolution_fs > 0 {
        resolution_fs as u64
    } else {
        1
    };
    let data = Arc::new(StaticSimData {
        hierarchy,
        id_to_handle: Arc::new(id_store.id_to_handle),
        simulator_name: vhpi::simulator_name().unwrap_or_else(|| "unknown".to_string()),
        simulator_version: vhpi::simulator_version().unwrap_or_else(|| "unknown".to_string()),
        time_precision: resolution_fs.ilog10() as i32 - 15,
    });
    if SIMULATOR_DATA.set(Arc::clone(&data)).is_err() {
        plugin_message("Simulator data was already initialized");
        return;
    }

    let Some(runtime) = RUNTIME_HANDLE.get() else {
        plugin_message("Runtime handle not set, cannot spawn message loop");
        return;
    };
    if !no_info {
        let command = format!("surfer sim --address {bind_address}");
        plugin_message(format!("To connect to this simulator, run: {command}"));
    }
    runtime.spawn(message_loop(Arc::clone(&data), bind_address));

    // Register the time-step callback now that the simulation is running
    let _ = register_cb(CbReason::NextTimeStep, next_time_step);

    // Block here until message is received
    plugin_message("Waiting for Surfer to connect...");
    debug_message("waiting for RunSimulation message");
    if !wait_for_run_command() {
        return;
    }
    debug_message("simulation started");
}

/// This runs every timestep that has a change or something important is happening
/// Send current simulation time to Surfer every [`INTERVAL_MS`] of wall-clock time.
fn next_time_step(_data: &CbData) {
    if !require_simulator_thread("next-time-step callback") {
        return;
    }
    if process_pending_simulator_commands() {
        return;
    }

    let now_ms = now_millis();
    let last_time_ms = LAST_SEND_TIME_MS.load(Ordering::Relaxed);

    if now_ms - last_time_ms >= INTERVAL_MS {
        LAST_SEND_TIME_MS.store(now_ms, Ordering::Relaxed);

        if let Some(connection) = CLIENT_CONNECTION.get()
            && let Some(time) = protocol_time_from_vhpi(vhpi::get_time())
        {
            connection.send(SimulatorToSurferMessage::CurrentSimulationTime { time });
        }
    }

    // Register callback for next REAL simulation event
    let _ = register_cb(CbReason::NextTimeStep, next_time_step);
}

/// This runs when the simulation is complete, sends all data over to Surfer
fn end_of_sim(_data: &CbData) {
    if !require_simulator_thread("end-of-simulation callback") {
        return;
    }
    // Tells the plugin that the simulation is done
    SIMULATION_DONE.store(true, Ordering::SeqCst);
    {
        let Ok(mut value_changes) = VALUE_CHANGES
            .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
            .lock()
        else {
            plugin_message("Failed to lock VALUE_CHANGES mutex");
            return;
        };

        // We need some way to inform Surfer when the simulation is done
        if let Some(timestamp) = protocol_time_from_vhpi(vhpi::get_time()) {
            value_changes.ensure_time_step(SimulationTimestamp::from_ticks(timestamp));
        }
    }
    send_changed_values(SimulationStatus::Done);
    debug_message(
        "End of simulation reached. All values sent to Surfer. Plugin will now block indefinitely.",
    );
    wait_for_finish_command();
}

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
            if SIMULATION_DONE.load(Ordering::SeqCst) {
                //Simulation is done so we don't want so simulate any more time
                SimulatorToSurferMessage::ValueChanges {
                    time_steps: [].to_vec(),
                    status: SimulationStatus::Done,
                }
            } else {
                if let Some(delay) = maybe_delay {
                    let Ok(now) = SimulationTimestamp::try_from(vhpi::get_time().to_i64()) else {
                        plugin_message("Rejecting negative VHPI simulation timestamp");
                        return SimulatorToSurferMessage::Acknowledge {
                            success: false,
                            error: Some("Rejecting negative VHPI simulation timestamp".into()),
                        };
                    };
                    let Some(duration) = delay.checked_delay(now, i64::MAX as u64) else {
                        plugin_message("Run deadline is in the past or exceeds VHPI time range");
                        return SimulatorToSurferMessage::Acknowledge {
                            success: false,
                            error: Some(
                                "Run deadline is in the past or exceeds VHPI time range".into(),
                            ),
                        };
                    };
                    let Ok(delay) = duration.to_i64() else {
                        return SimulatorToSurferMessage::Acknowledge {
                            success: false,
                            error: Some("Run duration exceeds VHPI time range".into()),
                        };
                    };
                    let success = if delay == 0 {
                        register_end_of_time_step_callback()
                    } else {
                        register_timeout_callback(Time::from(delay))
                    };
                    if !success {
                        return SimulatorToSurferMessage::Acknowledge {
                            success: false,
                            error: Some("Failed to register simulation run callback".into()),
                        };
                    }
                }
                SimulatorToSurferMessage::Acknowledge {
                    success: true,
                    error: None,
                }
            }
        }
        SurferToSimulatorMessage::PauseSimulation => {
            // Pause at the end of the current timestep, replacing any timed callback.
            let success = register_end_of_time_step_callback();
            SimulatorToSurferMessage::Acknowledge {
                success,
                error: (!success).then(|| "Failed to register simulation pause callback".into()),
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
            debug_message("shutdown received from Surfer, closing plugin connection");
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
            let result = SIMULATOR_DATA.get().map_or(
                SimulatorToSurferMessage::Acknowledge {
                    success: false,
                    error: Some("Simulator data unavailable".into()),
                },
                |data| handle_surfer_message(&message, data),
            );
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
            let _ = vhpi::control(Control::Finish);
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
    // Called on the simulator thread while startup, pause, or end-of-simulation
    // is waiting. Tokio workers remain free to receive and enqueue the request.
    runtime.block_on(receiver.recv())
}

// Startup and pause callbacks wait here until a successful RunSimulation command
// is processed on the simulator thread.
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
    // While simulation is active, the next-time-step callback polls without
    // blocking so queued requests run on the simulator thread between timesteps.
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

/// Set up all necessary info so the simulation can start
#[unsafe(no_mangle)]
pub extern "C" fn vhpi_plugin_startup() {
    if SIMULATOR_THREAD_ID.set(thread::current().id()).is_err() {
        eprintln!("VHPI plugin startup called more than once");
        return;
    }
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

    // Set up communication over TCP
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            plugin_message(format!("Failed to create Tokio runtime: {err:?}"));
            return;
        }
    };
    RUNTIME_HANDLE.set(runtime.handle().clone()).ok();

    Box::leak(Box::new(runtime));

    let _ = register_cb(CbReason::StartOfSimulation, start_of_sim);
    let _ = register_cb(CbReason::EndOfSimulation, end_of_sim);
}

fn get_startup_argument_values() -> (String, bool) {
    let tool = vhpi::handle(OneToOne::Tool);
    let arguments = tool.iterator(OneToMany::Argvs).map(|argv| {
        argv.get_str(StrProperty::StrVal)
            .or_else(|| argv.get_str(StrProperty::Name))
            .unwrap_or_default()
    });
    let options = StartupOptions::parse(arguments);
    DEBUG_ENABLED.store(options.debug, Ordering::Relaxed);
    printf!("Binding simulator plugin to {}", options.address);
    (options.address, options.no_info)
}

fn set_callback_on_signal(id: SimId, id_to_handle: &IdToHandleMap) {
    // Function that finds the input signals, puts a value changed callback on it and sends it's start value to the VALUE_CHANGED vector
    let Some(callback_store) = CALLBACK_STORE.get() else {
        printf!("Callback store unavailable while tracking signal {:?}", id);
        return;
    };
    if callback_store.contains_callback(id) {
        printf!("Callback on {:?} already exists", id);
    } else if let Some(handle) = id_to_handle.get(&id) {
        let _ = handle.with_handle(|sig_handle| {
            // Lock the inner Handle to register the callback
            match sig_handle.register_cb(CbReason::ValueChange, move |data| {
                record_value_change(data.obj(), id);
            }) {
                Ok(callback_handle) => {
                    callback_store.insert_callback(id, SafeHandle::new(callback_handle));
                }
                Err(err) => {
                    printf!("Failed to register callback: {:?}", err);
                }
            }
            // Call value_function with a locked reference to the handle
            record_value_change(sig_handle, id);
        });
    } else {
        printf!("Signal handle not found for {:?}", id);
    }
}

/// Unregister multiple signals from writing changes
fn remove_callbacks(sig_ids: &[SimId]) {
    let Some(callback_store) = CALLBACK_STORE.get() else {
        printf!("Callback store unavailable while untracking signals");
        return;
    };
    let time_steps = VALUE_CHANGES
        .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
        .lock()
        .map_err(|_| printf!("Failed to lock VALUE_CHANGES mutex while untracking signals"))
        .ok();
    let Some(mut time_steps) = time_steps else {
        return;
    };

    let sig_ids: HashSet<SimId> = sig_ids.iter().copied().collect();

    for &sig_id in &sig_ids {
        if let Some(callback_handle) = callback_store.remove_callback(sig_id) {
            let _ = callback_handle.with_handle(|locked_handle| {
                if let Err(err) = vhpi::remove_cb(locked_handle) {
                    plugin_message(format!("Failed to remove callback: {err:?}"));
                }
            });
        } else {
            printf!("No callback found for {:?}", sig_id);
        }
    }

    time_steps.remove_signals(&sig_ids);
}

// Finds the value of the signal when ValueChanged has been called and then creates a SignalChanged for that change
fn record_value_change(signal_handle: &Handle, id: SimId) {
    if !require_simulator_thread("value-change callback") {
        return;
    }
    let Ok(mut global_changes) = VALUE_CHANGES
        .get_or_init(|| Mutex::new(ValueChangeBuffer::default()))
        .lock()
    else {
        printf!("Failed to lock VALUE_CHANGES mutex");
        return;
    };

    if let Some(value) = vhpi_signal_value_to_vcd_bytes(signal_handle) {
        if let Some(timestamp) = protocol_time_from_vhpi(vhpi::get_time()) {
            global_changes.record(
                SimulationTimestamp::from_ticks(timestamp),
                id,
                SignalValue::VCDValue(value),
            );
        }
    }
}

fn int_to_bitvec_chars(value: u64, bits: usize) -> Vec<u8> {
    uint_to_bit_string(value, bits).into_bytes()
}

fn enum_value_to_vcd_bytes(signal: &Handle, ordinal: u32) -> Option<Vec<u8>> {
    let literals = signal.handle(OneToOne::Type).enum_literals()?;
    let literal = literals.get(ordinal as usize)?;
    let mut value = vec![b's'];
    value.extend_from_slice(literal.as_bytes());
    Some(value)
}

/// Convert the value from VHPI to VCD-bytes that can be sent to surfer
/// Signal handle is passed so that we can access enum-kinds
fn vhpi_signal_value_to_vcd_bytes(sig: &Handle) -> Option<Vec<u8>> {
    // TODO: Time and Physical should be encoded as strings
    let value = &sig.get_value(Format::ObjType).ok()?;

    match value {
        // TODO: Validate that this is the right approach
        Value::BinStr(s) => {
            let mut buf = Vec::with_capacity(s.len() + 1);
            buf.push(b'b');
            buf.extend_from_slice(s.as_bytes());
            Some(buf)
        }

        // TODO: Validate that this is the right approach
        // Expand each octal digit to 3 binary chars
        Value::OctStr(s) => {
            let mut buf = vec![b'b'];
            for ch in s.chars() {
                let digit = ch.to_digit(8)? as u8;
                buf.push(if digit & 4 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 2 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 1 != 0 { b'1' } else { b'0' });
            }
            Some(buf)
        }

        // TODO: Validate that this is the right approach
        // Expand each hex digit to 4 binary chars
        Value::HexStr(s) => {
            let mut buf = vec![b'b'];
            for ch in s.chars() {
                let digit = ch.to_digit(16)? as u8;
                buf.push(if digit & 8 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 4 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 2 != 0 { b'1' } else { b'0' });
                buf.push(if digit & 1 != 0 { b'1' } else { b'0' });
            }
            Some(buf)
        }

        // TODO: Validate that this is the right approach
        // Parse decimal and convert to binary
        Value::DecStr(_s) => {
            // let n: u64 = s.parse().ok()?;
            // let mut buf = vec![b'b'];
            // buf.extend_from_slice(&int_to_bitvec_chars(n, signal_len as usize));
            // Some(buf)
            todo!()
        }

        // Char is really just a kind of enum, I think?
        Value::Char(c) => enum_value_to_vcd_bytes(sig, *c as u32),

        Value::Int(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 32));
            Some(buf)
        }

        Value::LongInt(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 64));
            Some(buf)
        }

        // TODO: Make human readable, but how?
        Value::SmallPhysical(n) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(*n as u64, 32));
            Some(buf)
        }

        // TODO: Make human readable, but how?
        Value::Physical(p) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(p.to_i64() as u64, 64));
            Some(buf)
        }

        // TODO: Make human readable, but how?
        Value::Time(t) => {
            let mut buf = vec![b'b'];
            buf.extend_from_slice(&int_to_bitvec_chars(t.to_i64() as u64, 64));
            Some(buf)
        }

        Value::Logic(v) => Some(vec![u8::from(*v)]),

        Value::LogicVec(vals) => {
            let mut buf = Vec::with_capacity(vals.len() + 1);
            buf.push(b'b');
            for v in vals.iter() {
                buf.push(u8::from(*v));
            }
            Some(buf)
        }

        Value::SmallEnum(v) => enum_value_to_vcd_bytes(sig, u32::from(*v)),

        Value::Enum(v) => enum_value_to_vcd_bytes(sig, *v),

        Value::Real(f) => {
            let mut buf = vec![b'r'];
            buf.extend_from_slice(f.to_string().as_bytes());
            Some(buf)
        }

        Value::Str(s) => {
            let mut buf = Vec::with_capacity(s.len() + 1);
            buf.push(b's');
            buf.extend_from_slice(s.as_bytes());
            Some(buf)
        }

        // These don't have single-signal encodings, they should split into separate signals in the hierarchy
        Value::IntVec(_)
        | Value::LongIntVec(_)
        | Value::SmallPhysicalVec(_)
        | Value::PhysicalVec(_)
        | Value::TimeVec(_)
        | Value::SmallEnumVec(_)
        | Value::EnumVec(_)
        | Value::RealVec(_) => None,

        Value::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_time_from_vhpi_rejects_negative_time() {
        assert_eq!(protocol_time_from_vhpi(Time::from(-1_i64)), None);
        assert_eq!(protocol_time_from_vhpi(Time::from(i64::MIN)), None);
        assert_eq!(protocol_time_from_vhpi(Time::from(42_i64)), Some(42));
    }

    #[test]
    fn int_to_bitvec_chars_respects_width_and_order() {
        assert_eq!(int_to_bitvec_chars(0b1010, 4), b"1010");
        assert_eq!(int_to_bitvec_chars(0b1_1010, 4), b"1010");
        assert_eq!(int_to_bitvec_chars(0, 1), b"0");
    }

    #[test]
    fn dim_info_index_at_handles_up_and_down_ranges() {
        let up = DimInfo {
            left: 2,
            size: 3,
            is_up: true,
        };
        assert_eq!(up.index_at(0), 2);
        assert_eq!(up.index_at(1), 3);
        assert_eq!(up.index_at(2), 4);

        let down = DimInfo {
            left: 4,
            size: 3,
            is_up: false,
        };
        assert_eq!(down.index_at(0), 4);
        assert_eq!(down.index_at(1), 3);
        assert_eq!(down.index_at(2), 2);
    }

    #[test]
    fn linear_to_nd_indices_maps_row_major_positions() {
        let dims = vec![
            DimInfo {
                left: 0,
                size: 2,
                is_up: true,
            },
            DimInfo {
                left: 3,
                size: 3,
                is_up: false,
            },
        ];

        assert_eq!(linear_to_nd_indices(0, &dims), vec![0, 3]);
        assert_eq!(linear_to_nd_indices(1, &dims), vec![0, 2]);
        assert_eq!(linear_to_nd_indices(2, &dims), vec![0, 1]);
        assert_eq!(linear_to_nd_indices(3, &dims), vec![1, 3]);
        assert_eq!(linear_to_nd_indices(5, &dims), vec![1, 1]);
    }

    #[test]
    fn linear_to_nd_indices_handles_empty_and_zero_sized_dims() {
        assert!(linear_to_nd_indices(10, &[]).is_empty());

        let dims = vec![DimInfo {
            left: 7,
            size: 0,
            is_up: true,
        }];
        assert_eq!(linear_to_nd_indices(5, &dims), vec![0]);
    }

    #[test]
    fn array_index_name_formats_dimensions_as_leaf_names() {
        assert_eq!(array_index_name(&["0".to_string()]), "[0]");
        assert_eq!(
            array_index_name(&["0".to_string(), "0".to_string()]),
            "[0, 0]"
        );
    }

    #[test]
    fn logic_var_type_from_name_maps_predefined_types() {
        assert_eq!(
            logic_var_type_from_name("std_logic", false),
            Some(VarType::StdLogic)
        );
        assert_eq!(
            logic_var_type_from_name("STD_ULOGIC", false),
            Some(VarType::StdULogic)
        );
        assert_eq!(logic_var_type_from_name("bit", false), Some(VarType::Bit));
        assert_eq!(
            logic_var_type_from_name("std_logic_vector", true),
            Some(VarType::StdLogicVector)
        );
        assert_eq!(
            logic_var_type_from_name("std_ulogic_vector", true),
            Some(VarType::StdULogicVector)
        );
        assert_eq!(
            logic_var_type_from_name("bit_vector", true),
            Some(VarType::BitVector)
        );
        assert_eq!(logic_var_type_from_name("custom_logic", false), None);
    }
}
