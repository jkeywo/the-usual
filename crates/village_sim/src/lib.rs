//! Authoritative, deterministic village simulation.
//!
//! The first executable slice deliberately keeps the world small: it loads a
//! scenario and its people from RON, assigns stable runtime identifiers, and
//! advances an explicit fixed tick. Later slices add movement and actions to
//! the same scheduling contract.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
    fs,
    path::Path,
};

use bevy_ecs::{component::Component, entity::Entity, world::World};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The authoritative simulation interval. Presentation speed changes how
/// quickly ticks are consumed, never this value.
pub const TICK_DURATION_MS: u64 = 250;

/// The number of ticks that advance the in-world clock by one minute. One
/// in-world minute per tick makes a full day 1440 ticks; the value is a fixed
/// simulation semantic, unrelated to presentation speed.
pub const TICKS_PER_IN_WORLD_MINUTE: u64 = 1;

const MINUTES_PER_DAY: u32 = 24 * 60;

/// A wall-clock time of day within a single 24-hour village day. The clock is
/// deterministic: it is a pure function of the tick and the scenario's authored
/// starting time, never of presentation speed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    /// Builds a time of day from minutes since midnight, wrapping across a day.
    #[must_use]
    pub fn from_minute_of_day(minutes: u32) -> Self {
        let minutes = minutes % MINUTES_PER_DAY;
        Self {
            hour: (minutes / 60) as u8,
            minute: (minutes % 60) as u8,
        }
    }

    /// Minutes elapsed since midnight.
    #[must_use]
    pub fn minute_of_day(self) -> u32 {
        u32::from(self.hour) * 60 + u32::from(self.minute)
    }
}

fn default_start_time_of_day() -> TimeOfDay {
    TimeOfDay { hour: 8, minute: 0 }
}

/// A readable authored identifier, such as `person.newcomer_a`.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DefinitionId(pub String);

impl DefinitionId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// A stable, monotonic identifier for a spawned simulation instance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SimId(pub u64);

/// A caller-stable identifier for an order in the player's household queue.
///
/// The client chooses this value before submitting a command, so later
/// cancellation does not depend on an implementation-private ECS entity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PlayerTaskId(pub u64);

/// A resident definition authored in the people content domain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersonDefinition {
    pub id: DefinitionId,
    pub display_name: String,
    #[serde(default)]
    pub needs: Vec<NeedDefinition>,
    #[serde(default)]
    pub conducts: Vec<DefinitionId>,
}

/// The intentionally small first need schema. More needs will use the same
/// authored shape once Cottage Contention is proven.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NeedDefinition {
    pub kind: NeedKind,
    pub initial: u8,
    pub decay_per_tick: u8,
    pub activate_at: u8,
    pub retain_above: u8,
    pub recovery: u8,
    /// The value at or above which an autonomous need escalates to the Urgent
    /// band and preempts the household's player task queue. It defaults high
    /// so a need that omits it never escalates.
    #[serde(default = "default_urgent_at")]
    pub urgent_at: u8,
}

fn default_urgent_at() -> u8 {
    u8::MAX
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NeedKind {
    Toilet,
}

/// All people definitions loaded for one scenario.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeopleAsset {
    pub people: Vec<PersonDefinition>,
}

/// A scenario authored separately from its people definitions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioDefinition {
    pub id: DefinitionId,
    pub seed: u64,
    pub people: Vec<DefinitionId>,
    pub map: DefinitionId,
    pub objects: Vec<DefinitionId>,
    pub placements: Vec<ResidentPlacement>,
    /// The in-world time the scenario begins at. Defaults to morning so older
    /// scenarios that omit it still load.
    #[serde(default = "default_start_time_of_day")]
    pub start_time_of_day: TimeOfDay,
}

/// An authored starting tile for a resident in a scenario.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResidentPlacement {
    pub person: DefinitionId,
    pub position: TilePosition,
}

/// A tile in the layered authoritative map.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TilePosition {
    pub floor: u8,
    pub x: i32,
    pub y: i32,
}

/// A single floor's navigable bounds and non-walkable tiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FloorDefinition {
    pub floor: u8,
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub blocked: Vec<TilePosition>,
}

/// An occupiable connection between two map tiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PortalDefinition {
    pub id: DefinitionId,
    pub from: TilePosition,
    pub to: TilePosition,
    pub traversal_ticks: u8,
}

/// A data-authored layered map.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MapAsset {
    pub id: DefinitionId,
    pub floors: Vec<FloorDefinition>,
    pub portals: Vec<PortalDefinition>,
}

/// A capability exclusively used by a primitive while it is executing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Capability {
    Hands,
}

/// A named, exclusive slot on a smart object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectSlotDefinition {
    pub id: DefinitionId,
}

/// One executable interaction exposed by a data-defined smart object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectAffordanceDefinition {
    pub id: DefinitionId,
    pub slot: DefinitionId,
    pub capability: Capability,
    pub duration_ticks: u8,
}

/// An object instance placed in the authored map.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmartObjectDefinition {
    pub id: DefinitionId,
    pub object_type: DefinitionId,
    pub position: TilePosition,
    pub slots: Vec<ObjectSlotDefinition>,
    pub affordances: Vec<ObjectAffordanceDefinition>,
}

/// All smart object instances loaded for one scenario.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectsAsset {
    pub objects: Vec<SmartObjectDefinition>,
}

/// A conduct maps a behaviour slot to an authored plan. Slice five supports
/// only the Human toilet method; the priority field preserves the intended
/// conduct-resolution contract without prematurely building generic HTN.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConductAsset {
    pub id: DefinitionId,
    pub methods: Vec<ConductMethodDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConductMethodDefinition {
    pub slot: DefinitionId,
    pub plan: DefinitionId,
    pub priority: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanAsset {
    pub id: DefinitionId,
    pub object: DefinitionId,
    pub affordance: DefinitionId,
    pub priority: i32,
}

/// The content required to construct a scenario runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioContent {
    pub scenario: ScenarioDefinition,
    pub people: PeopleAsset,
    pub map: MapAsset,
    pub objects: ObjectsAsset,
    pub conducts: Vec<ConductAsset>,
    pub plans: Vec<PlanAsset>,
}

impl ScenarioContent {
    /// Loads the initial domain-separated RON fixture from a content root.
    pub fn load_cottage_arrival(content_root: impl AsRef<Path>) -> Result<Self, ContentError> {
        let content_root = content_root.as_ref();
        let scenario = load_ron(content_root.join("scenarios/cottage_arrival.ron"))?;
        let people = load_ron(content_root.join("people/newcomers.ron"))?;
        let map = load_ron(content_root.join("maps/cottage.ron"))?;
        let objects = load_ron(content_root.join("objects/cottage.ron"))?;
        let conducts = vec![load_ron(content_root.join("conducts/human.ron"))?];
        let plans = vec![load_ron(content_root.join("plans/toilet_need.ron"))?];
        Ok(Self {
            scenario,
            people,
            map,
            objects,
            conducts,
            plans,
        })
    }
}

fn load_ron<T: for<'de> Deserialize<'de>>(path: impl AsRef<Path>) -> Result<T, ContentError> {
    let path = path.as_ref();
    let source = fs::read_to_string(path).map_err(|source| ContentError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    ron::from_str(&source).map_err(|source| ContentError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// Errors loading or resolving authored scenario content.
#[derive(Debug, Error)]
pub enum ContentError {
    #[error("could not read content file {path}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse RON content file {path}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    #[error("scenario references missing person definition {0:?}")]
    MissingPerson(DefinitionId),
    #[error("scenario references map {expected:?}, but loaded map is {actual:?}")]
    WrongMap {
        expected: DefinitionId,
        actual: DefinitionId,
    },
    #[error("scenario is missing a placement for person definition {0:?}")]
    MissingPlacement(DefinitionId),
    #[error("scenario references missing object definition {0:?}")]
    MissingObject(DefinitionId),
    #[error("placement for {person:?} is not a walkable map tile: {position:?}")]
    InvalidPlacement {
        person: DefinitionId,
        position: TilePosition,
    },
    #[error(
        "placement for {person:?} conflicts with {occupied_by:?} at occupied tile {position:?}"
    )]
    DuplicatePlacement {
        person: DefinitionId,
        occupied_by: DefinitionId,
        position: TilePosition,
    },
    #[error("person {person:?} references missing conduct {conduct:?}")]
    MissingConduct {
        person: DefinitionId,
        conduct: DefinitionId,
    },
    #[error("conduct {conduct:?} has no Human toilet method")]
    MissingToiletMethod { conduct: DefinitionId },
    #[error("conduct {conduct:?} references missing toilet plan {plan:?}")]
    MissingToiletPlan {
        conduct: DefinitionId,
        plan: DefinitionId,
    },
    #[error("toilet plan {plan:?} names an invalid object affordance")]
    InvalidToiletPlan { plan: DefinitionId },
}

/// The resident component retained on the authoritative ECS world.
#[derive(Component, Debug)]
struct Resident {
    definition_id: DefinitionId,
    display_name: String,
}

#[derive(Component, Debug)]
struct Position(TilePosition);

/// The arbitration band an action competes in. Bands change effective
/// priority only; the deterministic tie-break (priority, request age, SimId)
/// is unchanged. `Mandatory` is reserved for accident consequences and is not
/// yet produced by any system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionBand {
    Autonomous,
    Player,
    Urgent,
    // Reserved for accident consequences that interrupt everything; no system
    // produces it yet.
    #[allow(dead_code)]
    Mandatory,
}

impl ActionBand {
    /// The band's contribution to effective priority. Higher bands dominate
    /// any fine priority difference within a lower band.
    fn base(self) -> i32 {
        match self {
            Self::Autonomous => 0,
            Self::Player => 1_000,
            Self::Urgent => 2_000,
            Self::Mandatory => 3_000,
        }
    }
}

/// One durable order held in a resident's player task queue. Only the queue
/// head is dispatched into the execution engine at a time.
#[derive(Clone, Debug)]
struct QueuedPlayerTask {
    task: PlayerTaskId,
    order: QueuedOrder,
}

#[derive(Clone, Debug)]
enum QueuedOrder {
    GoTo {
        destination: TilePosition,
        priority: i32,
    },
    UseToilet {
        object: DefinitionId,
        affordance: DefinitionId,
        priority: i32,
    },
}

#[derive(Clone, Debug)]
struct GoToState {
    destination: TilePosition,
    path: Vec<TilePosition>,
    next_step: usize,
    traversal: Option<PortalTraversal>,
    priority: i32,
    band: ActionBand,
    request_age: u64,
    player_task: Option<PlayerTaskId>,
}

#[derive(Clone, Debug)]
struct PortalTraversal {
    portal: DefinitionId,
    destination: TilePosition,
    ticks_remaining: u8,
}

#[derive(Clone, Debug)]
struct UseRequest {
    resident: SimId,
    object: DefinitionId,
    affordance: DefinitionId,
    priority: i32,
    band: ActionBand,
    request_age: u64,
    player_task: Option<PlayerTaskId>,
}

/// One live claim to enter a tile this tick. Movement makes the same
/// deterministic arbitration promise as smart-object slots.
#[derive(Clone, Copy, Debug)]
struct MovementClaim {
    resident: SimId,
    target: TilePosition,
    priority: i32,
    band: ActionBand,
    request_age: u64,
}

#[derive(Clone, Debug)]
struct ActiveObjectUse {
    object: DefinitionId,
    affordance: DefinitionId,
    slot: DefinitionId,
    capability: Capability,
    ticks_remaining: u8,
    player_task: Option<PlayerTaskId>,
}

#[derive(Clone, Debug)]
struct ToiletNeedState {
    value: u8,
    decay_per_tick: u8,
    activate_at: u8,
    retain_above: u8,
    recovery: u8,
    urgent_at: u8,
}

#[derive(Clone, Debug)]
struct ToiletPlan {
    object: DefinitionId,
    affordance: DefinitionId,
    priority: i32,
}

/// A typed order submitted by the presentation client. Commands enter the
/// authoritative inbox immediately but are only validated during the next
/// tick's ingest phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum PlayerCommand {
    QueueGoTo {
        task: PlayerTaskId,
        resident: SimId,
        destination: TilePosition,
        priority: i32,
    },
    QueueUseToilet {
        task: PlayerTaskId,
        resident: SimId,
        object: DefinitionId,
        affordance: DefinitionId,
        priority: i32,
    },
    CancelPlayerTask {
        task: PlayerTaskId,
    },
}

impl PlayerCommand {
    #[must_use]
    pub fn task(&self) -> PlayerTaskId {
        match self {
            Self::QueueGoTo { task, .. }
            | Self::QueueUseToilet { task, .. }
            | Self::CancelPlayerTask { task } => *task,
        }
    }
}

/// Why a next-tick player command receipt was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum PlayerCommandRejection {
    DuplicateTask,
    InvalidMoveTarget,
    InvalidUseTarget,
    ResidentBusy,
    UnknownResident,
    UnknownTask,
    TaskNotCancellable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlayerTaskState {
    Queued,
    Active,
    /// Preempted by an urgent need; the order stays at its resident's queue
    /// head and resumes once the need is satisfied.
    Paused,
    Completed,
    Cancelled,
}

fn compare_use_requests(left: &UseRequest, right: &UseRequest) -> std::cmp::Ordering {
    (right.band.base() + right.priority)
        .cmp(&(left.band.base() + left.priority))
        .then_with(|| left.request_age.cmp(&right.request_age))
        .then_with(|| left.resident.cmp(&right.resident))
}

fn compare_movement_claims(left: &MovementClaim, right: &MovementClaim) -> std::cmp::Ordering {
    (right.band.base() + right.priority)
        .cmp(&(left.band.base() + left.priority))
        .then_with(|| left.request_age.cmp(&right.request_age))
        .then_with(|| left.resident.cmp(&right.resident))
}

/// Immutable outcome emitted by the authoritative simulation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorldEvent {
    pub tick: u64,
    pub kind: WorldEventKind,
}

/// Semantic event kinds available in the initial runner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum WorldEventKind {
    TickCompleted {
        /// A keyed deterministic draw, included in this minimal runner so
        /// replay tests exercise the seed contract before gameplay actions
        /// begin using it.
        keyed_marker: u64,
    },
    GoToArrived {
        resident: SimId,
        destination: TilePosition,
    },
    GoToWaited {
        resident: SimId,
        blocked_by: SimId,
        position: TilePosition,
    },
    GoToFailed {
        resident: SimId,
        destination: TilePosition,
    },
    GoToCancelled {
        task: PlayerTaskId,
        resident: SimId,
        destination: TilePosition,
    },
    ObjectUseStarted {
        resident: SimId,
        object: DefinitionId,
        affordance: DefinitionId,
    },
    ObjectUseWaited {
        resident: SimId,
        object: DefinitionId,
        affordance: DefinitionId,
        blocked_by: SimId,
    },
    ObjectUseCompleted {
        resident: SimId,
        object: DefinitionId,
        affordance: DefinitionId,
    },
    ToiletNeedRecovered {
        resident: SimId,
        value: u8,
    },
    PlayerCommandAccepted {
        task: PlayerTaskId,
    },
    PlayerCommandRejected {
        task: PlayerTaskId,
        reason: PlayerCommandRejection,
    },
    TaskCancelled {
        task: PlayerTaskId,
        resident: SimId,
        object: DefinitionId,
        affordance: DefinitionId,
    },
}

fn keyed_draw(seed: u64, tick: u64, event_id: u64, purpose: u64) -> u64 {
    // A stable integer mixer rather than `Hash`, whose implementation is not
    // a simulation compatibility contract. Later action code supplies a
    // distinct event ID and purpose for each authored random decision.
    let mut value = seed
        ^ tick.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ event_id.wrapping_mul(0xD1B5_4A32_D192_ED03)
        ^ purpose.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// A deterministic, fixed-tick authoritative simulation.
pub struct Simulation {
    world: World,
    seed: u64,
    tick: u64,
    start_minute_of_day: u32,
    next_sim_id: u64,
    residents: BTreeMap<SimId, Entity>,
    map: MapAsset,
    objects: BTreeMap<DefinitionId, SmartObjectDefinition>,
    occupancy: BTreeMap<TilePosition, SimId>,
    portal_occupancy: BTreeMap<DefinitionId, SimId>,
    go_to: BTreeMap<SimId, GoToState>,
    use_requests: Vec<UseRequest>,
    active_object_uses: BTreeMap<SimId, ActiveObjectUse>,
    toilet_needs: BTreeMap<SimId, ToiletNeedState>,
    autonomous_toilet_plans: BTreeMap<SimId, ToiletPlan>,
    autonomous_toilet_intentions: BTreeSet<SimId>,
    urgent_toilet_intentions: BTreeSet<SimId>,
    player_command_inbox: Vec<PlayerCommand>,
    player_tasks: BTreeMap<PlayerTaskId, PlayerTaskState>,
    player_task_queue: BTreeMap<SimId, VecDeque<QueuedPlayerTask>>,
    object_slot_claims: BTreeMap<(DefinitionId, DefinitionId), SimId>,
    capability_claims: BTreeMap<(SimId, Capability), DefinitionId>,
    pending_events: Vec<WorldEvent>,
    ingested_events: Vec<WorldEvent>,
    event_ledger: Vec<WorldEvent>,
}

/// The small, immutable read model consumed by the Cottage client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CottageSnapshot {
    pub tick: u64,
    pub time_of_day: TimeOfDay,
    pub floors: Vec<FloorDefinition>,
    pub objects: Vec<SmartObjectDefinition>,
    pub residents: Vec<ClientResidentSnapshot>,
}

/// A resident's presentation-safe state. ECS entity IDs remain private.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientResidentSnapshot {
    pub id: SimId,
    pub definition_id: DefinitionId,
    pub display_name: String,
    pub position: TilePosition,
    /// The current authoritative scalar need, when the resident owns one.
    pub toilet_need: Option<u8>,
    /// The autonomous intention currently selected by the simulation.
    pub autonomous_intention: Option<ClientIntention>,
    /// The resident's ordered player task queue, head first. Each entry is
    /// individually cancellable by its stable id.
    pub player_tasks: Vec<ClientPlayerTaskSnapshot>,
}

/// Presentation-safe names for the narrow set of autonomous intentions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientIntention {
    Toilet,
}

/// Presentation-safe state of player work that is still in progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientPlayerTaskSnapshot {
    pub id: PlayerTaskId,
    pub state: ClientPlayerTaskState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientPlayerTaskState {
    Queued,
    Active,
    Paused,
}

impl Simulation {
    /// Creates a simulation from validated scenario content.
    pub fn from_content(content: ScenarioContent) -> Result<Self, ContentError> {
        let definitions = content
            .people
            .people
            .into_iter()
            .map(|person| (person.id.clone(), person))
            .collect::<BTreeMap<_, _>>();

        if content.scenario.map != content.map.id {
            return Err(ContentError::WrongMap {
                expected: content.scenario.map,
                actual: content.map.id,
            });
        }

        let objects = content
            .objects
            .objects
            .into_iter()
            .map(|object| (object.id.clone(), object))
            .collect::<BTreeMap<_, _>>();
        for object_id in &content.scenario.objects {
            if !objects.contains_key(object_id) {
                return Err(ContentError::MissingObject(object_id.clone()));
            }
        }

        let conducts = content
            .conducts
            .into_iter()
            .map(|conduct| (conduct.id.clone(), conduct))
            .collect::<BTreeMap<_, _>>();
        let plans = content
            .plans
            .into_iter()
            .map(|plan| (plan.id.clone(), plan))
            .collect::<BTreeMap<_, _>>();

        let mut simulation = Self {
            world: World::new(),
            seed: content.scenario.seed,
            tick: 0,
            start_minute_of_day: content.scenario.start_time_of_day.minute_of_day(),
            next_sim_id: 1,
            residents: BTreeMap::new(),
            map: content.map,
            objects,
            occupancy: BTreeMap::new(),
            portal_occupancy: BTreeMap::new(),
            go_to: BTreeMap::new(),
            use_requests: Vec::new(),
            active_object_uses: BTreeMap::new(),
            toilet_needs: BTreeMap::new(),
            autonomous_toilet_plans: BTreeMap::new(),
            autonomous_toilet_intentions: BTreeSet::new(),
            urgent_toilet_intentions: BTreeSet::new(),
            player_command_inbox: Vec::new(),
            player_tasks: BTreeMap::new(),
            player_task_queue: BTreeMap::new(),
            object_slot_claims: BTreeMap::new(),
            capability_claims: BTreeMap::new(),
            pending_events: Vec::new(),
            ingested_events: Vec::new(),
            event_ledger: Vec::new(),
        };

        for definition_id in content.scenario.people {
            let person = definitions
                .get(&definition_id)
                .ok_or_else(|| ContentError::MissingPerson(definition_id.clone()))?;
            let placement = content
                .scenario
                .placements
                .iter()
                .find(|placement| placement.person == definition_id)
                .ok_or_else(|| ContentError::MissingPlacement(definition_id.clone()))?;
            if !simulation.is_walkable(placement.position) {
                return Err(ContentError::InvalidPlacement {
                    person: definition_id,
                    position: placement.position,
                });
            }
            if let Some(occupied_by) = simulation.occupancy.get(&placement.position) {
                let occupied_by = simulation.residents[occupied_by];
                let occupied_by = simulation
                    .world
                    .get::<Resident>(occupied_by)
                    .expect("resident index always points into the ECS world")
                    .definition_id
                    .clone();
                return Err(ContentError::DuplicatePlacement {
                    person: definition_id,
                    occupied_by,
                    position: placement.position,
                });
            }
            let sim_id = simulation.spawn_resident(person, placement.position);
            if let Some(need) = person
                .needs
                .iter()
                .find(|need| need.kind == NeedKind::Toilet)
            {
                let plan =
                    resolve_human_toilet_plan(person, &conducts, &plans, &simulation.objects)?;
                simulation.toilet_needs.insert(
                    sim_id,
                    ToiletNeedState {
                        value: need.initial,
                        decay_per_tick: need.decay_per_tick,
                        activate_at: need.activate_at,
                        retain_above: need.retain_above,
                        recovery: need.recovery,
                        urgent_at: need.urgent_at,
                    },
                );
                simulation.autonomous_toilet_plans.insert(sim_id, plan);
            }
        }

        Ok(simulation)
    }

    fn spawn_resident(&mut self, person: &PersonDefinition, position: TilePosition) -> SimId {
        let sim_id = SimId(self.next_sim_id);
        self.next_sim_id += 1;
        let entity = self
            .world
            .spawn(Resident {
                definition_id: person.id.clone(),
                display_name: person.display_name.clone(),
            })
            .insert(Position(position))
            .id();
        self.residents.insert(sim_id, entity);
        self.occupancy.insert(position, sim_id);
        sim_id
    }

    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The deterministic in-world time of day, derived from the tick and the
    /// scenario's authored starting time.
    #[must_use]
    pub fn time_of_day(&self) -> TimeOfDay {
        let elapsed_minutes = self.tick / TICKS_PER_IN_WORLD_MINUTE;
        let minute_of_day =
            (u64::from(self.start_minute_of_day) + elapsed_minutes) % u64::from(MINUTES_PER_DAY);
        TimeOfDay::from_minute_of_day(minute_of_day as u32)
    }

    /// Returns residents in stable `SimId` order, keeping Bevy entity IDs
    /// private to the implementation.
    #[must_use]
    pub fn residents(&self) -> Vec<(SimId, DefinitionId)> {
        self.residents
            .iter()
            .map(|(sim_id, entity)| {
                let resident = self
                    .world
                    .get::<Resident>(*entity)
                    .expect("resident index always points into the ECS world");
                (*sim_id, resident.definition_id.clone())
            })
            .collect()
    }

    /// Events available to systems during the current tick's ingest phase.
    #[must_use]
    pub fn ingested_events(&self) -> &[WorldEvent] {
        &self.ingested_events
    }

    /// The complete immutable event ledger, in publication order.
    #[must_use]
    pub fn event_ledger(&self) -> &[WorldEvent] {
        &self.event_ledger
    }

    /// Returns a resident's authoritative tile position.
    #[must_use]
    pub fn resident_position(&self, resident: SimId) -> Option<TilePosition> {
        self.residents.get(&resident).and_then(|entity| {
            self.world
                .get::<Position>(*entity)
                .map(|position| position.0)
        })
    }

    /// Projects only authored, presentation-safe Cottage data for a client.
    #[must_use]
    pub fn cottage_snapshot(&self) -> CottageSnapshot {
        let residents = self
            .residents
            .iter()
            .map(|(id, entity)| {
                let resident = self
                    .world
                    .get::<Resident>(*entity)
                    .expect("resident index always points into the ECS world");
                let position = self
                    .world
                    .get::<Position>(*entity)
                    .expect("resident always has a position")
                    .0;
                let player_tasks = self.client_player_tasks(*id);
                ClientResidentSnapshot {
                    id: *id,
                    definition_id: resident.definition_id.clone(),
                    display_name: resident.display_name.clone(),
                    position,
                    toilet_need: self.toilet_need(*id),
                    autonomous_intention: self
                        .has_autonomous_toilet_intention(*id)
                        .then_some(ClientIntention::Toilet),
                    player_tasks,
                }
            })
            .collect();
        CottageSnapshot {
            tick: self.tick,
            time_of_day: self.time_of_day(),
            floors: self.map.floors.clone(),
            objects: self.objects.values().cloned().collect(),
            residents,
        }
    }

    /// Projects a resident's player task queue, head first, as presentation
    /// state. The durable order is the queue itself; execution engine maps are
    /// only consulted for each task's live Queued/Active/Paused state.
    fn client_player_tasks(&self, resident: SimId) -> Vec<ClientPlayerTaskSnapshot> {
        let Some(queue) = self.player_task_queue.get(&resident) else {
            return Vec::new();
        };
        queue
            .iter()
            .filter_map(|queued| {
                let state = match self.player_tasks.get(&queued.task)? {
                    PlayerTaskState::Queued => ClientPlayerTaskState::Queued,
                    PlayerTaskState::Active => ClientPlayerTaskState::Active,
                    PlayerTaskState::Paused => ClientPlayerTaskState::Paused,
                    PlayerTaskState::Completed | PlayerTaskState::Cancelled => return None,
                };
                Some(ClientPlayerTaskSnapshot {
                    id: queued.task,
                    state,
                })
            })
            .collect()
    }

    /// Starts a narrow navigation task. It plans immediately but claims no
    /// future tiles or portals; each next step is resolved against live state.
    pub fn begin_go_to(&mut self, resident: SimId, destination: TilePosition) -> bool {
        self.begin_go_to_with_priority(resident, destination, 0)
    }

    /// Starts navigation with an explicit claim priority. When multiple
    /// residents approach the same live tile in one tick, higher priority,
    /// then the older request, then lower `SimId` gets the tile.
    pub fn begin_go_to_with_priority(
        &mut self,
        resident: SimId,
        destination: TilePosition,
        priority: i32,
    ) -> bool {
        self.begin_go_to_banded(resident, destination, priority, ActionBand::Player, None)
    }

    fn begin_go_to_banded(
        &mut self,
        resident: SimId,
        destination: TilePosition,
        priority: i32,
        band: ActionBand,
        player_task: Option<PlayerTaskId>,
    ) -> bool {
        let Some(origin) = self.resident_position(resident) else {
            return false;
        };
        let path = self.find_path(origin, destination).unwrap_or_default();
        self.go_to.insert(
            resident,
            GoToState {
                destination,
                path,
                next_step: 1,
                traversal: None,
                priority,
                band,
                request_age: self.tick,
                player_task,
            },
        );
        true
    }

    /// Requests an object affordance. Requests are deliberately claim-free:
    /// slots and capabilities are acquired only during the execution phase of
    /// a later tick.
    pub fn begin_use_object(
        &mut self,
        resident: SimId,
        object: &DefinitionId,
        affordance: &DefinitionId,
        priority: i32,
    ) -> bool {
        self.begin_use_object_banded(
            resident,
            object,
            affordance,
            priority,
            ActionBand::Player,
            None,
        )
    }

    fn begin_use_object_banded(
        &mut self,
        resident: SimId,
        object: &DefinitionId,
        affordance: &DefinitionId,
        priority: i32,
        band: ActionBand,
        player_task: Option<PlayerTaskId>,
    ) -> bool {
        if !self.residents.contains_key(&resident)
            || self.active_object_uses.contains_key(&resident)
            || self
                .use_requests
                .iter()
                .any(|request| request.resident == resident)
        {
            return false;
        }
        let Some(object_definition) = self.objects.get(object) else {
            return false;
        };
        if !object_definition
            .affordances
            .iter()
            .any(|candidate| candidate.id == *affordance)
        {
            return false;
        }
        self.use_requests.push(UseRequest {
            resident,
            object: object.clone(),
            affordance: affordance.clone(),
            priority,
            band,
            request_age: self.tick,
            player_task,
        });
        true
    }

    /// Places a typed player order into the inbox for validation during the
    /// next tick. Submission itself never grants object or capability claims.
    pub fn submit_player_command(&mut self, command: PlayerCommand) {
        self.player_command_inbox.push(command);
    }

    /// Returns the resident currently executing in a named smart-object slot.
    #[must_use]
    pub fn object_slot_claimant(
        &self,
        object: &DefinitionId,
        slot: &DefinitionId,
    ) -> Option<SimId> {
        self.object_slot_claims
            .get(&(object.clone(), slot.clone()))
            .copied()
    }

    /// Returns whether an executing primitive currently claims a capability.
    #[must_use]
    pub fn capability_claimant(
        &self,
        resident: SimId,
        capability: Capability,
    ) -> Option<DefinitionId> {
        self.capability_claims.get(&(resident, capability)).cloned()
    }

    /// Returns the bounded Toilet need value, where higher means more urgent.
    #[must_use]
    pub fn toilet_need(&self, resident: SimId) -> Option<u8> {
        self.toilet_needs.get(&resident).map(|need| need.value)
    }

    /// Test and debug support for setting the narrow first autonomous need.
    pub fn set_toilet_need(&mut self, resident: SimId, value: u8) -> bool {
        let Some(need) = self.toilet_needs.get_mut(&resident) else {
            return false;
        };
        need.value = value;
        true
    }

    #[must_use]
    pub fn has_autonomous_toilet_intention(&self, resident: SimId) -> bool {
        self.autonomous_toilet_intentions.contains(&resident)
    }

    /// Consumes exactly one deterministic 250 ms simulation tick.
    ///
    /// Events published by a preceding tick first become observable here;
    /// this tick's own event is deferred until the next call.
    pub fn advance_tick(&mut self) {
        self.ingested_events = std::mem::take(&mut self.pending_events);
        self.tick += 1;

        self.ingest_player_commands();
        self.update_toilet_needs();
        self.choose_autonomous_toilet_plans();
        self.dispatch_player_queues();
        self.tick_go_to();
        self.tick_object_uses();

        let event = WorldEvent {
            tick: self.tick,
            kind: WorldEventKind::TickCompleted {
                keyed_marker: keyed_draw(self.seed, self.tick, self.tick, 0),
            },
        };
        self.event_ledger.push(event.clone());
        self.pending_events.push(event);
    }

    fn update_toilet_needs(&mut self) {
        for need in self.toilet_needs.values_mut() {
            need.value = need.value.saturating_add(need.decay_per_tick);
        }
    }

    fn choose_autonomous_toilet_plans(&mut self) {
        let residents = self.toilet_needs.keys().copied().collect::<Vec<_>>();
        for resident in residents {
            let need = self
                .toilet_needs
                .get(&resident)
                .expect("resident was collected from toilet needs");
            let (value, activate_at, retain_above, urgent_at) = (
                need.value,
                need.activate_at,
                need.retain_above,
                need.urgent_at,
            );

            let selected = self.autonomous_toilet_intentions.contains(&resident);
            if selected && value <= retain_above {
                self.autonomous_toilet_intentions.remove(&resident);
                self.urgent_toilet_intentions.remove(&resident);
                continue;
            }
            if !selected && value < activate_at {
                continue;
            }
            self.autonomous_toilet_intentions.insert(resident);

            // Escalate to, or relax from, the Urgent band using the same
            // hysteresis the intention itself uses to remain selected.
            if value >= urgent_at {
                self.urgent_toilet_intentions.insert(resident);
            } else if value <= retain_above {
                self.urgent_toilet_intentions.remove(&resident);
            }
            let urgent = self.urgent_toilet_intentions.contains(&resident);

            let plan = self
                .autonomous_toilet_plans
                .get(&resident)
                .expect("every toilet need has a resolved conduct plan")
                .clone();

            if urgent {
                // Urgent overrides the player queue: pause whatever player order
                // holds the resident's slot (unless it is already the toilet),
                // then drive the toilet plan in the Urgent band. The paused
                // order stays at its queue head and resumes on recovery.
                if !self.resident_engaged_with_toilet(resident, &plan) {
                    self.pause_player_head_for_urgent(resident);
                }
                self.drive_toilet_plan(resident, &plan, ActionBand::Urgent);
                continue;
            }

            // Non-urgent autonomy never preempts: it yields entirely to any
            // player queue, and to work already in flight for the resident.
            if self.resident_has_player_queue(resident)
                || !self.resident_execution_slot_free(resident)
            {
                continue;
            }
            self.drive_toilet_plan(resident, &plan, ActionBand::Autonomous);
        }
    }

    /// Drives the two-phase toilet plan in a given band: first physically
    /// reach the object tile, then enter the ordinary claim/use path.
    fn drive_toilet_plan(&mut self, resident: SimId, plan: &ToiletPlan, band: ActionBand) {
        let destination = self
            .objects
            .get(&plan.object)
            .expect("resolved toilet plan names a loaded object")
            .position;
        if self.resident_position(resident) != Some(destination) {
            if !self.go_to.contains_key(&resident) {
                let _ = self.begin_go_to_banded(resident, destination, plan.priority, band, None);
            }
            return;
        }
        if self.active_object_uses.contains_key(&resident)
            || self
                .use_requests
                .iter()
                .any(|request| request.resident == resident)
        {
            return;
        }
        let _ = self.begin_use_object_banded(
            resident,
            &plan.object,
            &plan.affordance,
            plan.priority,
            band,
            None,
        );
    }

    /// Whether the resident is already physically engaged with the toilet plan
    /// — walking to it, requesting it, or actively using it — so an urgent
    /// need need not preempt anything.
    fn resident_engaged_with_toilet(&self, resident: SimId, plan: &ToiletPlan) -> bool {
        let toilet_position = self.objects.get(&plan.object).map(|object| object.position);
        if let Some(state) = self.go_to.get(&resident)
            && Some(state.destination) == toilet_position
        {
            return true;
        }
        if let Some(active) = self.active_object_uses.get(&resident)
            && active.object == plan.object
            && active.affordance == plan.affordance
        {
            return true;
        }
        self.use_requests.iter().any(|request| {
            request.resident == resident
                && request.object == plan.object
                && request.affordance == plan.affordance
        })
    }

    /// Releases whatever dispatched player order holds the resident's execution
    /// slot at its safe boundary and marks it Paused, leaving it at the queue
    /// head to resume once the urgent need is satisfied.
    fn pause_player_head_for_urgent(&mut self, resident: SimId) {
        if let Some(task) = self
            .go_to
            .get(&resident)
            .and_then(|state| state.player_task)
        {
            self.go_to.remove(&resident);
            self.player_tasks.insert(task, PlayerTaskState::Paused);
            return;
        }
        if let Some(task) = self
            .active_object_uses
            .get(&resident)
            .and_then(|active| active.player_task)
        {
            let active = self
                .active_object_uses
                .remove(&resident)
                .expect("active object use was just observed");
            self.object_slot_claims
                .remove(&(active.object.clone(), active.slot.clone()));
            self.capability_claims
                .remove(&(resident, active.capability));
            self.player_tasks.insert(task, PlayerTaskState::Paused);
            return;
        }
        if let Some(index) = self
            .use_requests
            .iter()
            .position(|request| request.resident == resident && request.player_task.is_some())
        {
            let request = self.use_requests.remove(index);
            if let Some(task) = request.player_task {
                self.player_tasks.insert(task, PlayerTaskState::Paused);
            }
        }
    }

    fn resident_has_player_queue(&self, resident: SimId) -> bool {
        self.player_task_queue
            .get(&resident)
            .is_some_and(|queue| !queue.is_empty())
    }

    fn resident_execution_slot_free(&self, resident: SimId) -> bool {
        !self.go_to.contains_key(&resident)
            && !self.active_object_uses.contains_key(&resident)
            && !self
                .use_requests
                .iter()
                .any(|request| request.resident == resident)
    }

    fn is_task_dispatched(&self, resident: SimId, task: PlayerTaskId) -> bool {
        self.go_to
            .get(&resident)
            .and_then(|state| state.player_task)
            == Some(task)
            || self
                .active_object_uses
                .get(&resident)
                .and_then(|active| active.player_task)
                == Some(task)
            || self
                .use_requests
                .iter()
                .any(|request| request.player_task == Some(task))
    }

    /// Dispatches the head of each resident's player queue into the execution
    /// engine when the resident is free and not owned by an urgent need. Only
    /// one player task per resident executes at a time.
    fn dispatch_player_queues(&mut self) {
        let residents = self.player_task_queue.keys().copied().collect::<Vec<_>>();
        for resident in residents {
            if self.urgent_toilet_intentions.contains(&resident) {
                continue;
            }
            let Some(head) = self
                .player_task_queue
                .get(&resident)
                .and_then(|queue| queue.front())
                .cloned()
            else {
                continue;
            };
            if self.is_task_dispatched(resident, head.task)
                || !self.resident_execution_slot_free(resident)
            {
                continue;
            }
            match head.order {
                QueuedOrder::GoTo {
                    destination,
                    priority,
                } => {
                    if self.begin_go_to_banded(
                        resident,
                        destination,
                        priority,
                        ActionBand::Player,
                        Some(head.task),
                    ) {
                        self.player_tasks.insert(head.task, PlayerTaskState::Active);
                    }
                }
                QueuedOrder::UseToilet {
                    object,
                    affordance,
                    priority,
                } => {
                    if self.begin_use_object_banded(
                        resident,
                        &object,
                        &affordance,
                        priority,
                        ActionBand::Player,
                        Some(head.task),
                    ) {
                        self.player_tasks.insert(head.task, PlayerTaskState::Queued);
                    }
                }
            }
        }
    }

    fn locate_queued_task(&self, task: PlayerTaskId) -> Option<(SimId, QueuedOrder)> {
        self.player_task_queue.iter().find_map(|(resident, queue)| {
            queue
                .iter()
                .find(|queued| queued.task == task)
                .map(|queued| (*resident, queued.order.clone()))
        })
    }

    fn remove_task_from_queue(&mut self, resident: SimId, task: PlayerTaskId) {
        if let Some(queue) = self.player_task_queue.get_mut(&resident) {
            queue.retain(|queued| queued.task != task);
            if queue.is_empty() {
                self.player_task_queue.remove(&resident);
            }
        }
    }

    /// Records a player task's terminal state and drops it from its resident's
    /// queue so the next queued order can dispatch.
    fn finish_player_task(&mut self, resident: SimId, task: PlayerTaskId, state: PlayerTaskState) {
        self.player_tasks.insert(task, state);
        self.remove_task_from_queue(resident, task);
    }

    fn ingest_player_commands(&mut self) {
        let commands = std::mem::take(&mut self.player_command_inbox);
        for command in commands {
            match command {
                PlayerCommand::QueueGoTo {
                    task,
                    resident,
                    destination,
                    priority,
                } => self.ingest_queue_go_to(task, resident, destination, priority),
                PlayerCommand::QueueUseToilet {
                    task,
                    resident,
                    object,
                    affordance,
                    priority,
                } => self.ingest_queue_use_toilet(task, resident, object, affordance, priority),
                PlayerCommand::CancelPlayerTask { task } => self.ingest_cancel_player_task(task),
            }
        }
    }

    fn ingest_queue_go_to(
        &mut self,
        task: PlayerTaskId,
        resident: SimId,
        destination: TilePosition,
        priority: i32,
    ) {
        if self.player_tasks.contains_key(&task) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::DuplicateTask,
            });
            return;
        }
        if !self.residents.contains_key(&resident) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::UnknownResident,
            });
            return;
        }
        if !self.is_walkable(destination) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::InvalidMoveTarget,
            });
            return;
        }
        // A second order no longer conflicts: it joins the resident's queue and
        // dispatches once earlier orders finish or are cancelled.
        self.player_tasks.insert(task, PlayerTaskState::Queued);
        self.player_task_queue
            .entry(resident)
            .or_default()
            .push_back(QueuedPlayerTask {
                task,
                order: QueuedOrder::GoTo {
                    destination,
                    priority,
                },
            });
        self.emit(WorldEventKind::PlayerCommandAccepted { task });
    }

    fn ingest_queue_use_toilet(
        &mut self,
        task: PlayerTaskId,
        resident: SimId,
        object: DefinitionId,
        affordance: DefinitionId,
        priority: i32,
    ) {
        if self.player_tasks.contains_key(&task) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::DuplicateTask,
            });
            return;
        }
        if !self.residents.contains_key(&resident) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::UnknownResident,
            });
            return;
        }
        // This command represents one semantic household order, rather than
        // a generic "use any affordance" escape hatch. It must therefore use
        // the resident's conduct-resolved toilet plan exactly.
        let is_toilet_target = self
            .autonomous_toilet_plans
            .get(&resident)
            .is_some_and(|plan| plan.object == object && plan.affordance == affordance);
        if !is_toilet_target {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::InvalidUseTarget,
            });
            return;
        }

        // Player orders own the resident's next action, so a competing
        // autonomous request is naturally suppressed by band and by the
        // non-urgent yield in `choose_autonomous_toilet_plans`; a busy resident
        // simply queues the order behind its current work.
        self.player_tasks.insert(task, PlayerTaskState::Queued);
        self.player_task_queue
            .entry(resident)
            .or_default()
            .push_back(QueuedPlayerTask {
                task,
                order: QueuedOrder::UseToilet {
                    object,
                    affordance,
                    priority,
                },
            });
        self.emit(WorldEventKind::PlayerCommandAccepted { task });
    }

    fn ingest_cancel_player_task(&mut self, task: PlayerTaskId) {
        let Some(state) = self.player_tasks.get(&task).copied() else {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::UnknownTask,
            });
            return;
        };
        if !matches!(
            state,
            PlayerTaskState::Queued | PlayerTaskState::Active | PlayerTaskState::Paused
        ) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::TaskNotCancellable,
            });
            return;
        }
        let Some((resident, order)) = self.locate_queued_task(task) else {
            // A live task is always in exactly one resident's queue.
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::UnknownTask,
            });
            return;
        };

        self.emit(WorldEventKind::PlayerCommandAccepted { task });

        // Dispatched as the resident's active movement.
        if self
            .go_to
            .get(&resident)
            .and_then(|state| state.player_task)
            == Some(task)
        {
            let destination = self
                .go_to
                .get(&resident)
                .map(|state| state.destination)
                .expect("dispatched go-to was just observed");
            self.go_to.remove(&resident);
            self.finish_player_task(resident, task, PlayerTaskState::Cancelled);
            self.emit(WorldEventKind::GoToCancelled {
                task,
                resident,
                destination,
            });
            return;
        }
        // Dispatched as an active object use: release at the safe boundary.
        if self
            .active_object_uses
            .get(&resident)
            .and_then(|active| active.player_task)
            == Some(task)
        {
            let active = self
                .active_object_uses
                .remove(&resident)
                .expect("active object use was just observed");
            self.object_slot_claims
                .remove(&(active.object.clone(), active.slot.clone()));
            self.capability_claims
                .remove(&(resident, active.capability));
            self.finish_player_task(resident, task, PlayerTaskState::Cancelled);
            self.emit(WorldEventKind::TaskCancelled {
                task,
                resident,
                object: active.object,
                affordance: active.affordance,
            });
            return;
        }
        // Dispatched as a not-yet-claimed use request.
        if let Some(index) = self
            .use_requests
            .iter()
            .position(|request| request.player_task == Some(task))
        {
            let request = self.use_requests.remove(index);
            self.finish_player_task(resident, task, PlayerTaskState::Cancelled);
            self.emit(WorldEventKind::TaskCancelled {
                task,
                resident,
                object: request.object,
                affordance: request.affordance,
            });
            return;
        }

        // Not dispatched: a tail-queued or urgent-paused order. Remove it in
        // place; its resident keeps its remaining queue.
        self.finish_player_task(resident, task, PlayerTaskState::Cancelled);
        match order {
            QueuedOrder::GoTo { destination, .. } => {
                self.emit(WorldEventKind::GoToCancelled {
                    task,
                    resident,
                    destination,
                });
            }
            QueuedOrder::UseToilet {
                object, affordance, ..
            } => {
                self.emit(WorldEventKind::TaskCancelled {
                    task,
                    resident,
                    object,
                    affordance,
                });
            }
        }
    }

    fn tick_go_to(&mut self) {
        let mut claims = self
            .go_to
            .iter()
            .filter_map(|(resident, state)| {
                if state.traversal.is_some() {
                    return None;
                }
                let origin = self.resident_position(*resident)?;
                let target = state.path.get(state.next_step).copied()?;
                if self.occupancy.contains_key(&target)
                    || self
                        .portal_between(origin, target)
                        .is_some_and(|portal| self.portal_occupancy.contains_key(&portal.id))
                {
                    return None;
                }
                Some(MovementClaim {
                    resident: *resident,
                    target,
                    priority: state.priority,
                    band: state.band,
                    request_age: state.request_age,
                })
            })
            .collect::<Vec<_>>();
        claims.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| compare_movement_claims(left, right))
        });

        let mut approved = BTreeSet::new();
        let mut waited = BTreeMap::new();
        for claim in claims {
            if let Some(winner) = approved.iter().copied().find(|winner| {
                self.go_to
                    .get(winner)
                    .is_some_and(|state| state.path.get(state.next_step) == Some(&claim.target))
            }) {
                waited.insert(claim.resident, (winner, claim.target));
            } else {
                approved.insert(claim.resident);
            }
        }

        let residents = self.go_to.keys().copied().collect::<Vec<_>>();
        let mut processed = BTreeSet::new();
        for resident in approved {
            self.tick_one_go_to(resident);
            processed.insert(resident);
        }
        for resident in residents {
            if processed.contains(&resident) {
                continue;
            }
            if let Some((blocked_by, position)) = waited.get(&resident).copied() {
                self.emit(WorldEventKind::GoToWaited {
                    resident,
                    blocked_by,
                    position,
                });
            } else {
                self.tick_one_go_to(resident);
            }
        }
    }

    fn tick_object_uses(&mut self) {
        let active_residents = self.active_object_uses.keys().copied().collect::<Vec<_>>();
        for resident in active_residents {
            let mut active = self
                .active_object_uses
                .remove(&resident)
                .expect("active resident was collected from active uses");
            active.ticks_remaining = active.ticks_remaining.saturating_sub(1);
            if active.ticks_remaining == 0 {
                let recovers_toilet_need =
                    self.matches_resolved_toilet_plan(resident, &active.object, &active.affordance);
                self.object_slot_claims
                    .remove(&(active.object.clone(), active.slot.clone()));
                self.capability_claims
                    .remove(&(resident, active.capability));
                if let Some(task) = active.player_task {
                    self.finish_player_task(resident, task, PlayerTaskState::Completed);
                }
                self.emit(WorldEventKind::ObjectUseCompleted {
                    resident,
                    object: active.object,
                    affordance: active.affordance,
                });
                if recovers_toilet_need {
                    self.recover_toilet_need(resident);
                }
            } else {
                self.active_object_uses.insert(resident, active);
            }
        }

        self.use_requests.sort_by(compare_use_requests);
        let requests = std::mem::take(&mut self.use_requests);
        for request in requests {
            let Some(object) = self.objects.get(&request.object) else {
                continue;
            };
            let Some(affordance) = object
                .affordances
                .iter()
                .find(|candidate| candidate.id == request.affordance)
            else {
                continue;
            };
            let slot_key = (request.object.clone(), affordance.slot.clone());
            let capability_key = (request.resident, affordance.capability);
            let blocked_by = self.object_slot_claims.get(&slot_key).copied().or_else(|| {
                self.capability_claims
                    .get(&capability_key)
                    .map(|_| request.resident)
            });
            if let Some(blocked_by) = blocked_by {
                self.emit(WorldEventKind::ObjectUseWaited {
                    resident: request.resident,
                    object: request.object.clone(),
                    affordance: request.affordance.clone(),
                    blocked_by,
                });
                self.use_requests.push(request);
                continue;
            }

            self.object_slot_claims.insert(slot_key, request.resident);
            self.capability_claims
                .insert(capability_key, request.object.clone());
            self.active_object_uses.insert(
                request.resident,
                ActiveObjectUse {
                    object: request.object.clone(),
                    affordance: request.affordance.clone(),
                    slot: affordance.slot.clone(),
                    capability: affordance.capability,
                    ticks_remaining: affordance.duration_ticks.max(1),
                    player_task: request.player_task,
                },
            );
            if let Some(task) = request.player_task {
                self.player_tasks.insert(task, PlayerTaskState::Active);
            }
            self.emit(WorldEventKind::ObjectUseStarted {
                resident: request.resident,
                object: request.object,
                affordance: request.affordance,
            });
        }
    }

    fn recover_toilet_need(&mut self, resident: SimId) {
        let Some(value) = self.toilet_needs.get_mut(&resident).map(|need| {
            need.value = need.value.saturating_sub(need.recovery);
            need.value
        }) else {
            return;
        };
        self.emit(WorldEventKind::ToiletNeedRecovered { resident, value });
    }

    fn matches_resolved_toilet_plan(
        &self,
        resident: SimId,
        object: &DefinitionId,
        affordance: &DefinitionId,
    ) -> bool {
        self.autonomous_toilet_plans
            .get(&resident)
            .is_some_and(|plan| plan.object == *object && plan.affordance == *affordance)
    }

    fn tick_one_go_to(&mut self, resident: SimId) {
        let Some(mut state) = self.go_to.remove(&resident) else {
            return;
        };
        let origin = self
            .resident_position(resident)
            .expect("go-to state only exists for residents");

        if let Some(mut traversal) = state.traversal.take() {
            traversal.ticks_remaining = traversal.ticks_remaining.saturating_sub(1);
            if traversal.ticks_remaining == 0 {
                self.portal_occupancy.remove(&traversal.portal);
                self.move_resident(resident, origin, traversal.destination);
                state.next_step += 1;
                self.finish_or_continue_go_to(resident, state);
            } else {
                state.traversal = Some(traversal);
                self.go_to.insert(resident, state);
            }
            return;
        }

        if state.path.is_empty() {
            if let Some(task) = state.player_task {
                self.finish_player_task(resident, task, PlayerTaskState::Completed);
            }
            self.emit(WorldEventKind::GoToFailed {
                resident,
                destination: state.destination,
            });
            return;
        }
        if origin == state.destination {
            if let Some(task) = state.player_task {
                self.finish_player_task(resident, task, PlayerTaskState::Completed);
            }
            self.emit(WorldEventKind::GoToArrived {
                resident,
                destination: state.destination,
            });
            return;
        }

        let Some(next) = state.path.get(state.next_step).copied() else {
            if let Some(task) = state.player_task {
                self.finish_player_task(resident, task, PlayerTaskState::Completed);
            }
            self.emit(WorldEventKind::GoToFailed {
                resident,
                destination: state.destination,
            });
            return;
        };
        if let Some(blocked_by) = self.occupancy.get(&next).copied() {
            if let Some(path) =
                self.find_path_avoiding_occupied_tiles(resident, origin, state.destination)
            {
                // The route was made against static map geometry. A resident
                // can subsequently occupy its next step, so replace it with
                // a deterministic live route instead of needlessly waiting.
                state.path = path;
                state.next_step = 1;
                self.go_to.insert(resident, state);
                return;
            }
            self.emit(WorldEventKind::GoToWaited {
                resident,
                blocked_by,
                position: next,
            });
            self.go_to.insert(resident, state);
            return;
        }

        if let Some(portal) = self.portal_between(origin, next).cloned() {
            if let Some(blocked_by) = self.portal_occupancy.get(&portal.id).copied() {
                self.emit(WorldEventKind::GoToWaited {
                    resident,
                    blocked_by,
                    position: next,
                });
                self.go_to.insert(resident, state);
                return;
            }
            self.portal_occupancy.insert(portal.id.clone(), resident);
            state.traversal = Some(PortalTraversal {
                portal: portal.id,
                destination: next,
                ticks_remaining: portal.traversal_ticks.max(1),
            });
            self.go_to.insert(resident, state);
            return;
        }

        self.move_resident(resident, origin, next);
        state.next_step += 1;
        self.finish_or_continue_go_to(resident, state);
    }

    fn finish_or_continue_go_to(&mut self, resident: SimId, state: GoToState) {
        if self.resident_position(resident) == Some(state.destination) {
            if let Some(task) = state.player_task {
                self.finish_player_task(resident, task, PlayerTaskState::Completed);
            }
            self.emit(WorldEventKind::GoToArrived {
                resident,
                destination: state.destination,
            });
        } else {
            self.go_to.insert(resident, state);
        }
    }

    fn move_resident(&mut self, resident: SimId, origin: TilePosition, destination: TilePosition) {
        self.occupancy.remove(&origin);
        self.occupancy.insert(destination, resident);
        let entity = self.residents[&resident];
        self.world
            .entity_mut(entity)
            .get_mut::<Position>()
            .expect("resident has position")
            .0 = destination;
    }

    fn emit(&mut self, kind: WorldEventKind) {
        let event = WorldEvent {
            tick: self.tick,
            kind,
        };
        self.event_ledger.push(event.clone());
        self.pending_events.push(event);
    }

    fn is_walkable(&self, position: TilePosition) -> bool {
        self.map
            .floors
            .iter()
            .find(|floor| floor.floor == position.floor)
            .is_some_and(|floor| {
                position.x >= 0
                    && position.x < floor.width
                    && position.y >= 0
                    && position.y < floor.height
                    && !floor.blocked.contains(&position)
            })
    }

    fn portal_between(&self, from: TilePosition, to: TilePosition) -> Option<&PortalDefinition> {
        self.map.portals.iter().find(|portal| {
            (portal.from == from && portal.to == to) || (portal.to == from && portal.from == to)
        })
    }

    fn neighbours(&self, position: TilePosition) -> Vec<TilePosition> {
        let mut neighbours = [
            TilePosition {
                floor: position.floor,
                x: position.x - 1,
                y: position.y,
            },
            TilePosition {
                floor: position.floor,
                x: position.x + 1,
                y: position.y,
            },
            TilePosition {
                floor: position.floor,
                x: position.x,
                y: position.y - 1,
            },
            TilePosition {
                floor: position.floor,
                x: position.x,
                y: position.y + 1,
            },
        ]
        .into_iter()
        .filter(|candidate| self.is_walkable(*candidate))
        .collect::<Vec<_>>();
        for portal in &self.map.portals {
            if portal.from == position {
                neighbours.push(portal.to);
            } else if portal.to == position {
                neighbours.push(portal.from);
            }
        }
        neighbours.sort();
        neighbours
    }

    fn find_path(
        &self,
        origin: TilePosition,
        destination: TilePosition,
    ) -> Option<Vec<TilePosition>> {
        if !self.is_walkable(origin) || !self.is_walkable(destination) {
            return None;
        }
        let mut open = BinaryHeap::new();
        let mut costs = BTreeMap::from([(origin, 0_u32)]);
        let mut came_from = BTreeMap::new();
        open.push(Reverse((
            self.heuristic(origin, destination),
            0_u32,
            origin,
        )));
        while let Some(Reverse((_estimated, cost, current))) = open.pop() {
            if current == destination {
                let mut path = vec![current];
                let mut cursor = current;
                while let Some(previous) = came_from.get(&cursor).copied() {
                    path.push(previous);
                    cursor = previous;
                }
                path.reverse();
                return Some(path);
            }
            if costs.get(&current).copied() != Some(cost) {
                continue;
            }
            for neighbour in self.neighbours(current) {
                let next_cost = cost + 1;
                if costs.get(&neighbour).is_none_or(|known| next_cost < *known) {
                    costs.insert(neighbour, next_cost);
                    came_from.insert(neighbour, current);
                    open.push(Reverse((
                        next_cost + self.heuristic(neighbour, destination),
                        next_cost,
                        neighbour,
                    )));
                }
            }
        }
        None
    }

    /// Replans around tiles occupied by other residents. Initial routes stay
    /// occupancy-agnostic so simultaneous movement claims remain fair.
    fn find_path_avoiding_occupied_tiles(
        &self,
        resident: SimId,
        origin: TilePosition,
        destination: TilePosition,
    ) -> Option<Vec<TilePosition>> {
        if !self.is_walkable(origin)
            || !self.is_walkable(destination)
            || self
                .occupancy
                .get(&destination)
                .is_some_and(|occupant| *occupant != resident)
        {
            return None;
        }
        let mut open = BinaryHeap::new();
        let mut costs = BTreeMap::from([(origin, 0_u32)]);
        let mut came_from = BTreeMap::new();
        open.push(Reverse((
            self.heuristic(origin, destination),
            0_u32,
            origin,
        )));
        while let Some(Reverse((_estimated, cost, current))) = open.pop() {
            if current == destination {
                let mut path = vec![current];
                let mut cursor = current;
                while let Some(previous) = came_from.get(&cursor).copied() {
                    path.push(previous);
                    cursor = previous;
                }
                path.reverse();
                return Some(path);
            }
            if costs.get(&current).copied() != Some(cost) {
                continue;
            }
            for neighbour in self.neighbours(current).into_iter().filter(|candidate| {
                self.occupancy
                    .get(candidate)
                    .is_none_or(|occupant| *occupant == resident)
                    && !self.tile_is_next_movement_target(*candidate, resident)
            }) {
                let next_cost = cost + 1;
                if costs.get(&neighbour).is_none_or(|known| next_cost < *known) {
                    costs.insert(neighbour, next_cost);
                    came_from.insert(neighbour, current);
                    open.push(Reverse((
                        next_cost + self.heuristic(neighbour, destination),
                        next_cost,
                        neighbour,
                    )));
                }
            }
        }
        None
    }

    /// A rerouting resident must also avoid the next tile another resident
    /// has already committed to approaching. This is a soft, one-step
    /// reservation used only for replanning; normal movement claims still
    /// decide simultaneous approaches by priority.
    fn tile_is_next_movement_target(&self, tile: TilePosition, resident: SimId) -> bool {
        self.go_to.iter().any(|(other, state)| {
            *other != resident
                && state
                    .traversal
                    .as_ref()
                    .map(|traversal| traversal.destination == tile)
                    .unwrap_or_else(|| state.path.get(state.next_step) == Some(&tile))
        })
    }

    fn heuristic(&self, from: TilePosition, to: TilePosition) -> u32 {
        from.x.abs_diff(to.x) + from.y.abs_diff(to.y) + u32::from(from.floor != to.floor)
    }
}

fn resolve_human_toilet_plan(
    person: &PersonDefinition,
    conducts: &BTreeMap<DefinitionId, ConductAsset>,
    plans: &BTreeMap<DefinitionId, PlanAsset>,
    objects: &BTreeMap<DefinitionId, SmartObjectDefinition>,
) -> Result<ToiletPlan, ContentError> {
    let conduct_id = person
        .conducts
        .first()
        .ok_or_else(|| ContentError::MissingConduct {
            person: person.id.clone(),
            conduct: DefinitionId::new("conduct.human"),
        })?;
    let conduct = conducts
        .get(conduct_id)
        .ok_or_else(|| ContentError::MissingConduct {
            person: person.id.clone(),
            conduct: conduct_id.clone(),
        })?;
    let method = conduct
        .methods
        .iter()
        .filter(|method| method.slot == DefinitionId::new("conduct.use_toilet"))
        .max_by_key(|method| method.priority)
        .ok_or_else(|| ContentError::MissingToiletMethod {
            conduct: conduct.id.clone(),
        })?;
    let plan = plans
        .get(&method.plan)
        .ok_or_else(|| ContentError::MissingToiletPlan {
            conduct: conduct.id.clone(),
            plan: method.plan.clone(),
        })?;
    let is_valid = objects.get(&plan.object).is_some_and(|object| {
        object
            .affordances
            .iter()
            .any(|affordance| affordance.id == plan.affordance)
    });
    if !is_valid {
        return Err(ContentError::InvalidToiletPlan {
            plan: plan.id.clone(),
        });
    }
    Ok(ToiletPlan {
        object: plan.object.clone(),
        affordance: plan.affordance.clone(),
        priority: plan.priority + method.priority,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn content_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/content")
    }

    fn load_simulation() -> Simulation {
        let content = ScenarioContent::load_cottage_arrival(content_root()).expect("fixture loads");
        Simulation::from_content(content).expect("fixture resolves")
    }

    fn load_simulation_with_seed(seed: u64) -> Simulation {
        let mut content =
            ScenarioContent::load_cottage_arrival(content_root()).expect("fixture loads");
        content.scenario.seed = seed;
        Simulation::from_content(content).expect("fixture resolves")
    }

    fn add_non_toilet_object(simulation: &mut Simulation) -> DefinitionId {
        let id = DefinitionId::new("object.cottage_sink");
        simulation.objects.insert(
            id.clone(),
            SmartObjectDefinition {
                id: id.clone(),
                object_type: DefinitionId::new("object_type.sink"),
                position: TilePosition {
                    floor: 0,
                    x: 4,
                    y: 2,
                },
                slots: vec![ObjectSlotDefinition {
                    id: DefinitionId::new("slot.use"),
                }],
                affordances: vec![ObjectAffordanceDefinition {
                    id: DefinitionId::new("affordance.wash_hands"),
                    slot: DefinitionId::new("slot.use"),
                    capability: Capability::Hands,
                    duration_ticks: 1,
                }],
            },
        );
        id
    }

    #[test]
    fn in_world_clock_advances_one_minute_per_tick_from_authored_start() {
        let mut simulation = load_simulation();
        assert_eq!(
            simulation.time_of_day(),
            TimeOfDay {
                hour: 16,
                minute: 0
            }
        );

        for _ in 0..30 {
            simulation.advance_tick();
        }
        let expected = TimeOfDay {
            hour: 16,
            minute: 30,
        };
        assert_eq!(simulation.time_of_day(), expected);
        assert_eq!(simulation.cottage_snapshot().time_of_day, expected);
    }

    #[test]
    fn time_of_day_maps_and_wraps_across_a_day() {
        assert_eq!(
            TimeOfDay::from_minute_of_day(0),
            TimeOfDay { hour: 0, minute: 0 }
        );
        assert_eq!(
            TimeOfDay::from_minute_of_day(23 * 60 + 59),
            TimeOfDay {
                hour: 23,
                minute: 59
            }
        );
        // A full day plus 90 minutes wraps to 01:30.
        assert_eq!(
            TimeOfDay::from_minute_of_day(24 * 60 + 90),
            TimeOfDay {
                hour: 1,
                minute: 30
            }
        );
        let quiz = TimeOfDay {
            hour: 19,
            minute: 0,
        };
        assert_eq!(TimeOfDay::from_minute_of_day(quiz.minute_of_day()), quiz);
    }

    #[test]
    fn cottage_fixture_assigns_readable_definitions_and_monotonic_sim_ids() {
        let simulation = load_simulation();

        assert_eq!(simulation.seed(), 4_243);
        assert_eq!(
            simulation.residents(),
            vec![
                (SimId(1), DefinitionId::new("person.newcomer_a")),
                (SimId(2), DefinitionId::new("person.newcomer_b")),
            ]
        );
    }

    #[test]
    fn duplicate_initial_placements_are_rejected() {
        let mut content =
            ScenarioContent::load_cottage_arrival(content_root()).expect("fixture loads");
        content.scenario.placements[1].position = content.scenario.placements[0].position;

        assert!(matches!(
            Simulation::from_content(content),
            Err(ContentError::DuplicatePlacement {
                person,
                occupied_by,
                position: TilePosition {
                    floor: 0,
                    x: 1,
                    y: 1,
                },
            }) if person == DefinitionId::new("person.newcomer_b")
                && occupied_by == DefinitionId::new("person.newcomer_a")
        ));
    }

    #[test]
    fn published_events_are_only_observable_on_the_next_tick() {
        let mut simulation = load_simulation();

        simulation.advance_tick();
        assert!(simulation.ingested_events().is_empty());
        assert_eq!(simulation.event_ledger().len(), 1);

        simulation.advance_tick();
        assert_eq!(
            simulation.ingested_events(),
            &[WorldEvent {
                tick: 1,
                kind: WorldEventKind::TickCompleted {
                    keyed_marker: keyed_draw(4_243, 1, 1, 0),
                },
            }]
        );
    }

    #[test]
    fn fixed_seed_fixture_has_repeatable_seed_derived_events() {
        let mut first = load_simulation();
        let mut second = load_simulation();
        let mut different_seed = load_simulation_with_seed(8_484);

        for _ in 0..3 {
            first.advance_tick();
            second.advance_tick();
            different_seed.advance_tick();
        }

        assert_eq!(first.seed(), second.seed());
        assert_eq!(first.residents(), second.residents());
        assert_eq!(first.event_ledger(), second.event_ledger());
        assert_eq!(first.ingested_events(), second.ingested_events());
        assert_ne!(first.event_ledger(), different_seed.event_ledger());
    }

    #[test]
    fn go_to_uses_the_paired_stair_portal_and_emits_arrival() {
        let mut simulation = load_simulation();
        let resident = SimId(1);
        let destination = TilePosition {
            floor: 1,
            x: 6,
            y: 5,
        };

        assert!(simulation.begin_go_to(resident, destination));
        for _ in 0..11 {
            simulation.advance_tick();
        }

        assert_eq!(simulation.resident_position(resident), Some(destination));
        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToArrived {
                    resident,
                    destination,
                }
        }));
    }

    #[test]
    fn live_tile_occupancy_waits_without_reserving_future_tiles() {
        let mut simulation = load_simulation();
        let first = SimId(1);
        let second = SimId(2);

        assert!(simulation.begin_go_to(
            first,
            TilePosition {
                floor: 0,
                x: 3,
                y: 1,
            },
        ));
        assert!(simulation.begin_go_to(
            second,
            TilePosition {
                floor: 0,
                x: 2,
                y: 1,
            },
        ));
        simulation.advance_tick();

        assert_eq!(
            simulation.resident_position(first),
            Some(TilePosition {
                floor: 0,
                x: 2,
                y: 1,
            })
        );
        assert_eq!(
            simulation.resident_position(second),
            Some(TilePosition {
                floor: 0,
                x: 2,
                y: 2,
            })
        );
        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToWaited {
                    resident: second,
                    blocked_by: first,
                    position: TilePosition {
                        floor: 0,
                        x: 2,
                        y: 1,
                    },
                }
        }));
    }

    #[test]
    fn go_to_replans_when_another_resident_blocks_its_next_step() {
        let mut simulation = load_simulation();
        let resident = SimId(1);
        let blocker = SimId(2);

        assert!(simulation.begin_go_to(
            resident,
            TilePosition {
                floor: 0,
                x: 4,
                y: 1,
            },
        ));
        assert!(simulation.begin_go_to_with_priority(
            blocker,
            TilePosition {
                floor: 0,
                x: 3,
                y: 1,
            },
            1,
        ));
        simulation.advance_tick();
        assert_eq!(
            simulation.resident_position(blocker),
            Some(TilePosition {
                floor: 0,
                x: 2,
                y: 1,
            })
        );

        let route = simulation
            .find_path_avoiding_occupied_tiles(
                resident,
                TilePosition {
                    floor: 0,
                    x: 1,
                    y: 1,
                },
                TilePosition {
                    floor: 0,
                    x: 4,
                    y: 1,
                },
            )
            .expect("a route around the blocker and its immediate target");
        assert_eq!(
            route.first(),
            Some(&TilePosition {
                floor: 0,
                x: 1,
                y: 1,
            })
        );
        assert!(!route.contains(&TilePosition {
            floor: 0,
            x: 2,
            y: 1,
        }));
        assert!(!route.contains(&TilePosition {
            floor: 0,
            x: 3,
            y: 1,
        }));
    }

    #[test]
    fn higher_priority_approach_claim_wins_a_shared_live_tile() {
        let mut simulation = load_simulation();
        let target = TilePosition {
            floor: 0,
            x: 2,
            y: 1,
        };

        assert!(simulation.begin_go_to_with_priority(SimId(1), target, 1));
        assert!(simulation.begin_go_to_with_priority(SimId(2), target, 2));
        simulation.advance_tick();

        assert_eq!(
            simulation.resident_position(SimId(1)),
            Some(TilePosition {
                floor: 0,
                x: 1,
                y: 1,
            })
        );
        assert_eq!(simulation.resident_position(SimId(2)), Some(target));
        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToWaited {
                    resident: SimId(1),
                    blocked_by: SimId(2),
                    position: target,
                }
        }));
    }

    #[test]
    fn lower_sim_id_wins_an_equal_priority_and_age_shared_live_tile_claim() {
        let mut simulation = load_simulation();
        let target = TilePosition {
            floor: 0,
            x: 2,
            y: 1,
        };
        let lower_id = SimId(1);
        let higher_id = SimId(2);

        // Both requests start before the same tick, approach the same
        // unoccupied tile, and deliberately have equal priority and age.
        assert!(simulation.begin_go_to_with_priority(lower_id, target, 0));
        assert!(simulation.begin_go_to_with_priority(higher_id, target, 0));
        simulation.advance_tick();

        assert_eq!(simulation.resident_position(lower_id), Some(target));
        assert_eq!(
            simulation.resident_position(higher_id),
            Some(TilePosition {
                floor: 0,
                x: 2,
                y: 2,
            })
        );
        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToWaited {
                    resident: higher_id,
                    blocked_by: lower_id,
                    position: target,
                }
        }));
    }

    #[test]
    fn older_approach_claim_wins_a_shared_live_tile_before_sim_id() {
        let mut simulation = load_simulation();
        let target = TilePosition {
            floor: 0,
            x: 2,
            y: 1,
        };

        assert!(simulation.begin_go_to(SimId(1), target));
        assert!(simulation.begin_go_to(SimId(2), target));
        // Keep both approaches in the same contention tick while modelling a
        // newer request from resident 1 and an older request from resident 2.
        simulation
            .go_to
            .get_mut(&SimId(1))
            .expect("first resident has a navigation request")
            .request_age = simulation.tick + 1;
        simulation.advance_tick();

        assert_eq!(
            simulation.resident_position(SimId(1)),
            Some(TilePosition {
                floor: 0,
                x: 1,
                y: 1,
            })
        );
        assert_eq!(simulation.resident_position(SimId(2)), Some(target));
        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToWaited {
                    resident: SimId(1),
                    blocked_by: SimId(2),
                    position: target,
                }
        }));
    }

    #[test]
    fn impossible_go_to_emits_a_semantic_failure() {
        let mut simulation = load_simulation();
        let resident = SimId(1);
        let destination = TilePosition {
            floor: 1,
            x: 99,
            y: 99,
        };

        assert!(simulation.begin_go_to(resident, destination));
        simulation.advance_tick();
        simulation.advance_tick();

        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToFailed {
                    resident,
                    destination,
                }
        }));
    }

    fn toilet_ids() -> (DefinitionId, DefinitionId, DefinitionId) {
        (
            DefinitionId::new("object.cottage_toilet"),
            DefinitionId::new("affordance.use_toilet"),
            DefinitionId::new("slot.use"),
        )
    }

    #[test]
    fn object_requests_do_not_claim_a_slot_or_capability_before_execution() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();

        assert!(simulation.begin_use_object(SimId(1), &toilet, &affordance, 0));
        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), None);
        assert_eq!(
            simulation.capability_claimant(SimId(1), Capability::Hands),
            None
        );
    }

    #[test]
    fn contested_toilet_waits_then_retries_after_execution_releases_claims() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();
        let first = SimId(1);
        let second = SimId(2);

        assert!(simulation.begin_use_object(first, &toilet, &affordance, 0));
        assert!(simulation.begin_use_object(second, &toilet, &affordance, 0));
        simulation.advance_tick();

        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), Some(first));
        assert_eq!(
            simulation.capability_claimant(first, Capability::Hands),
            Some(toilet.clone())
        );
        assert_eq!(
            simulation.capability_claimant(second, Capability::Hands),
            None
        );

        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::ObjectUseWaited {
                    resident: second,
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                    blocked_by: first,
                }
        }));
        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(second)
        );
        assert_eq!(
            simulation.capability_claimant(first, Capability::Hands),
            None
        );
        assert_eq!(
            simulation.capability_claimant(second, Capability::Hands),
            Some(toilet)
        );
    }

    #[test]
    fn object_claim_order_is_priority_then_request_age_then_sim_id() {
        let (toilet, affordance, _) = toilet_ids();
        let request = |resident, priority, request_age| UseRequest {
            resident,
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority,
            band: ActionBand::Player,
            request_age,
            player_task: None,
        };

        assert_eq!(
            compare_use_requests(&request(SimId(2), 2, 9), &request(SimId(1), 1, 1)),
            std::cmp::Ordering::Less,
            "higher priority wins"
        );
        assert_eq!(
            compare_use_requests(&request(SimId(2), 1, 3), &request(SimId(1), 1, 4)),
            std::cmp::Ordering::Less,
            "older request wins equal priority"
        );
        assert_eq!(
            compare_use_requests(&request(SimId(1), 1, 3), &request(SimId(2), 1, 3)),
            std::cmp::Ordering::Less,
            "lower SimId wins a complete tie"
        );
    }

    #[test]
    fn higher_priority_request_wins_the_same_tick_contention() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();

        assert!(simulation.begin_use_object(SimId(1), &toilet, &affordance, 1));
        assert!(simulation.begin_use_object(SimId(2), &toilet, &affordance, 2));
        simulation.advance_tick();

        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(SimId(2))
        );
    }

    #[test]
    fn player_command_is_validated_next_tick_and_receipt_is_deferred() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();
        let task = PlayerTaskId(41);

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority: 3,
        });
        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), None);
        assert!(simulation.ingested_events().is_empty());

        simulation.advance_tick();
        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(SimId(1))
        );
        assert!(simulation.ingested_events().is_empty());
        assert!(
            simulation
                .event_ledger()
                .iter()
                .any(|event| { event.kind == WorldEventKind::PlayerCommandAccepted { task } })
        );

        simulation.advance_tick();
        assert!(
            simulation
                .ingested_events()
                .iter()
                .any(|event| { event.kind == WorldEventKind::PlayerCommandAccepted { task } })
        );
    }

    #[test]
    fn invalid_player_command_is_rejected_without_claims() {
        let mut simulation = load_simulation();
        let (_, affordance, slot) = toilet_ids();
        let task = PlayerTaskId(42);
        let invalid_object = DefinitionId::new("object.missing_toilet");

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: invalid_object,
            affordance,
            priority: 0,
        });
        simulation.advance_tick();

        assert_eq!(
            simulation.object_slot_claimant(&DefinitionId::new("object.cottage_toilet"), &slot),
            None
        );
        assert_eq!(
            simulation.capability_claimant(SimId(1), Capability::Hands),
            None
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::PlayerCommandRejected {
                    task,
                    reason: PlayerCommandRejection::InvalidUseTarget,
                }
        }));
    }

    #[test]
    fn player_toilet_order_rejects_a_valid_non_toilet_affordance() {
        let mut simulation = load_simulation();
        let sink = add_non_toilet_object(&mut simulation);
        let task = PlayerTaskId(420);

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: sink,
            affordance: DefinitionId::new("affordance.wash_hands"),
            priority: 0,
        });
        simulation.advance_tick();

        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::PlayerCommandRejected {
                    task,
                    reason: PlayerCommandRejection::InvalidUseTarget,
                }
        }));
    }

    #[test]
    fn cancelling_an_active_player_task_releases_claims_without_completion() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();
        let task = PlayerTaskId(43);

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority: 0,
        });
        simulation.advance_tick();
        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(SimId(1))
        );
        assert_eq!(
            simulation.capability_claimant(SimId(1), Capability::Hands),
            Some(toilet.clone())
        );

        simulation.submit_player_command(PlayerCommand::CancelPlayerTask { task });
        simulation.advance_tick();

        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), None);
        assert_eq!(
            simulation.capability_claimant(SimId(1), Capability::Hands),
            None
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::TaskCancelled {
                    task,
                    resident: SimId(1),
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));
        assert!(!simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::ObjectUseCompleted {
                    resident: SimId(1),
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));

        simulation.advance_tick();
        assert!(simulation.ingested_events().iter().any(|event| {
            event.kind
                == WorldEventKind::TaskCancelled {
                    task,
                    resident: SimId(1),
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));
    }

    #[test]
    fn queued_player_task_can_be_cancelled_before_execution() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();
        let task = PlayerTaskId(44);

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority: 0,
        });
        simulation.submit_player_command(PlayerCommand::CancelPlayerTask { task });
        simulation.advance_tick();

        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), None);
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::TaskCancelled {
                    task,
                    resident: SimId(1),
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));
    }

    #[test]
    fn autonomous_toilet_need_uses_human_plan_and_recovers_only_on_completion() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();

        assert!(simulation.set_toilet_need(SimId(1), 50));
        assert_eq!(simulation.toilet_need(SimId(1)), Some(50));
        simulation.advance_tick();

        assert!(simulation.has_autonomous_toilet_intention(SimId(1)));
        assert_eq!(simulation.toilet_need(SimId(1)), Some(51));
        assert_eq!(
            simulation.resident_position(SimId(1)),
            Some(TilePosition {
                floor: 0,
                x: 2,
                y: 1
            })
        );
        simulation.advance_tick();
        assert_eq!(
            simulation.resident_position(SimId(1)),
            Some(TilePosition {
                floor: 0,
                x: 3,
                y: 1
            })
        );
        simulation.advance_tick();
        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(SimId(1))
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::ObjectUseStarted {
                    resident: SimId(1),
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));

        simulation.advance_tick();
        assert_eq!(simulation.toilet_need(SimId(1)), Some(0));
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::ToiletNeedRecovered {
                    resident: SimId(1),
                    value: 0,
                }
        }));
    }

    #[test]
    fn cottage_snapshot_exposes_authoritative_status_for_a_resident_card() {
        let mut simulation = load_simulation();
        assert!(simulation.set_toilet_need(SimId(1), 50));
        simulation.advance_tick();

        let resident = &simulation.cottage_snapshot().residents[0];
        assert_eq!(resident.toilet_need, Some(51));
        assert_eq!(resident.autonomous_intention, Some(ClientIntention::Toilet));
        assert!(resident.player_tasks.is_empty());

        let mut simulation = load_simulation();
        let task = PlayerTaskId(47);
        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: DefinitionId::new("object.cottage_toilet"),
            affordance: DefinitionId::new("affordance.use_toilet"),
            priority: 100,
        });
        simulation.advance_tick();

        let resident = &simulation.cottage_snapshot().residents[0];
        assert_eq!(
            resident.player_tasks,
            vec![ClientPlayerTaskSnapshot {
                id: task,
                state: ClientPlayerTaskState::Active,
            }]
        );
    }

    #[test]
    fn non_toilet_object_completion_does_not_recover_toilet_need() {
        let mut simulation = load_simulation();
        let sink = add_non_toilet_object(&mut simulation);
        let wash_hands = DefinitionId::new("affordance.wash_hands");
        assert!(simulation.set_toilet_need(SimId(1), 30));

        assert!(simulation.begin_use_object(SimId(1), &sink, &wash_hands, 0));
        simulation.advance_tick();
        simulation.advance_tick();

        assert_eq!(simulation.toilet_need(SimId(1)), Some(32));
        assert!(!simulation.event_ledger().iter().any(|event| {
            matches!(
                event.kind,
                WorldEventKind::ToiletNeedRecovered {
                    resident: SimId(1),
                    ..
                }
            )
        }));
    }

    #[test]
    fn player_toilet_order_suppresses_a_competing_autonomous_request() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();
        let task = PlayerTaskId(55);

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident: SimId(1),
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority: 0,
        });
        simulation.advance_tick();

        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(SimId(1))
        );
        assert!(
            simulation
                .event_ledger()
                .iter()
                .any(|event| { event.kind == WorldEventKind::PlayerCommandAccepted { task } })
        );
        assert_eq!(
            simulation
                .event_ledger()
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    WorldEventKind::ObjectUseStarted {
                        resident: SimId(1),
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn deferred_player_ground_move_uses_the_live_navigation_path() {
        let mut simulation = load_simulation();
        let task = PlayerTaskId(56);
        let destination = TilePosition {
            floor: 0,
            x: 4,
            y: 1,
        };
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task,
            resident: SimId(1),
            destination,
            priority: 100,
        });

        for _ in 0..3 {
            simulation.advance_tick();
        }

        assert_eq!(simulation.resident_position(SimId(1)), Some(destination));
        assert!(
            simulation
                .event_ledger()
                .iter()
                .any(|event| { event.kind == WorldEventKind::PlayerCommandAccepted { task } })
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToArrived {
                    resident: SimId(1),
                    destination,
                }
        }));
    }

    #[test]
    fn toilet_intention_uses_its_own_hysteresis_before_completion_recovers_it() {
        let mut simulation = load_simulation();
        assert!(simulation.set_toilet_need(SimId(1), 50));

        simulation.advance_tick();
        assert!(simulation.has_autonomous_toilet_intention(SimId(1)));
        assert!(simulation.set_toilet_need(SimId(1), 21));

        for _ in 0..3 {
            simulation.advance_tick();
        }
        // It remains selected below the activation threshold because its
        // lower retention threshold belongs to the current intention.
        assert!(simulation.has_autonomous_toilet_intention(SimId(1)));
        assert_eq!(simulation.toilet_need(SimId(1)), Some(0));

        simulation.advance_tick();
        assert!(!simulation.has_autonomous_toilet_intention(SimId(1)));
    }

    fn assert_no_resident_overlap(simulation: &Simulation) {
        let positions = simulation
            .residents()
            .into_iter()
            .map(|(resident, _)| {
                (
                    resident,
                    simulation
                        .resident_position(resident)
                        .expect("every fixture resident has a position"),
                )
            })
            .collect::<Vec<_>>();
        assert_ne!(positions[0].1, positions[1].1, "residents never overlap");
    }

    fn advance_until_arrivals(
        simulation: &mut Simulation,
        first: (SimId, TilePosition),
        second: (SimId, TilePosition),
    ) {
        let mut first_arrival_was_deferred = false;
        let mut second_arrival_was_deferred = false;

        for _ in 0..64 {
            simulation.advance_tick();
            assert_no_resident_overlap(simulation);
            for event in simulation.ingested_events() {
                match &event.kind {
                    WorldEventKind::GoToArrived {
                        resident,
                        destination,
                    } if (*resident, *destination) == first => {
                        assert!(event.tick < simulation.tick());
                        first_arrival_was_deferred = true;
                    }
                    WorldEventKind::GoToArrived {
                        resident,
                        destination,
                    } if (*resident, *destination) == second => {
                        assert!(event.tick < simulation.tick());
                        second_arrival_was_deferred = true;
                    }
                    _ => {}
                }
            }
            if first_arrival_was_deferred && second_arrival_was_deferred {
                break;
            }
        }

        assert!(
            first_arrival_was_deferred,
            "first resident arrives by a deferred event"
        );
        assert!(
            second_arrival_was_deferred,
            "second resident arrives by a deferred event"
        );
        assert_eq!(simulation.resident_position(first.0), Some(first.1));
        assert_eq!(simulation.resident_position(second.0), Some(second.1));
    }

    fn run_cottage_contention_script() -> Vec<WorldEvent> {
        let mut simulation = load_simulation();
        let first = SimId(1);
        let second = SimId(2);
        let upstairs_first = TilePosition {
            floor: 1,
            x: 5,
            y: 6,
        };
        let upstairs_second = TilePosition {
            floor: 1,
            x: 6,
            y: 5,
        };
        let downstairs_first = TilePosition {
            floor: 0,
            x: 1,
            y: 1,
        };
        let downstairs_second = TilePosition {
            floor: 0,
            x: 2,
            y: 2,
        };

        assert!(simulation.begin_go_to(first, upstairs_first));
        assert!(simulation.begin_go_to(second, upstairs_second));
        advance_until_arrivals(
            &mut simulation,
            (first, upstairs_first),
            (second, upstairs_second),
        );

        assert!(simulation.begin_go_to(first, downstairs_first));
        assert!(simulation.begin_go_to(second, downstairs_second));
        advance_until_arrivals(
            &mut simulation,
            (first, downstairs_first),
            (second, downstairs_second),
        );

        // Object use is deliberately not coupled to resident position in
        // this MVP. The assertion below proves the claim protocol only;
        // spatial affordance reachability is a later slice.
        let (toilet, affordance, slot) = toilet_ids();
        let player_task = PlayerTaskId(600);
        assert!(simulation.begin_use_object(second, &toilet, &affordance, 0));
        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task: player_task,
            resident: first,
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority: 1,
        });
        simulation.advance_tick();
        assert_no_resident_overlap(&simulation);
        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), Some(first));
        assert_eq!(
            simulation.capability_claimant(first, Capability::Hands),
            Some(toilet.clone())
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::ObjectUseWaited {
                    resident: second,
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                    blocked_by: first,
                }
        }));

        simulation.submit_player_command(PlayerCommand::CancelPlayerTask { task: player_task });
        simulation.advance_tick();
        assert_no_resident_overlap(&simulation);
        assert_eq!(
            simulation.capability_claimant(first, Capability::Hands),
            None
        );
        assert_ne!(simulation.object_slot_claimant(&toilet, &slot), Some(first));
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::TaskCancelled {
                    task: player_task,
                    resident: first,
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));
        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(second)
        );

        simulation.advance_tick();
        assert_no_resident_overlap(&simulation);
        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), None);
        assert_eq!(
            simulation.capability_claimant(second, Capability::Hands),
            None
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::ObjectUseCompleted {
                    resident: second,
                    object: toilet.clone(),
                    affordance: affordance.clone(),
                }
        }));

        simulation.event_ledger().to_vec()
    }

    #[test]
    fn cottage_contention_fixture_is_deterministic_and_semantically_complete() {
        let first = run_cottage_contention_script();
        let second = run_cottage_contention_script();
        assert_eq!(first, second, "the full scripted fixture is repeatable");
    }

    fn resident_player_tasks(
        simulation: &Simulation,
        resident: SimId,
    ) -> Vec<ClientPlayerTaskSnapshot> {
        simulation
            .cottage_snapshot()
            .residents
            .into_iter()
            .find(|candidate| candidate.id == resident)
            .expect("resident is present in the snapshot")
            .player_tasks
    }

    #[test]
    fn two_queued_player_orders_execute_in_fifo_order() {
        let mut simulation = load_simulation();
        let resident = SimId(1);
        let first = PlayerTaskId(101);
        let second = PlayerTaskId(102);
        let far = TilePosition {
            floor: 0,
            x: 4,
            y: 1,
        };
        let home = TilePosition {
            floor: 0,
            x: 1,
            y: 1,
        };
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: first,
            resident,
            destination: far,
            priority: 0,
        });
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: second,
            resident,
            destination: home,
            priority: 0,
        });

        // Only the head executes; the rest of the list stays queued.
        simulation.advance_tick();
        assert_eq!(
            resident_player_tasks(&simulation, resident),
            vec![
                ClientPlayerTaskSnapshot {
                    id: first,
                    state: ClientPlayerTaskState::Active,
                },
                ClientPlayerTaskSnapshot {
                    id: second,
                    state: ClientPlayerTaskState::Queued,
                },
            ]
        );

        for _ in 0..16 {
            simulation.advance_tick();
        }
        assert_eq!(simulation.resident_position(resident), Some(home));
        assert!(resident_player_tasks(&simulation, resident).is_empty());

        // The head arrival is published strictly before the second order's.
        let arrivals = simulation
            .event_ledger()
            .iter()
            .filter_map(|event| match &event.kind {
                WorldEventKind::GoToArrived {
                    resident: arrived,
                    destination,
                } if *arrived == resident => Some(*destination),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(arrivals, vec![far, home]);
    }

    #[test]
    fn a_non_head_queued_task_cancels_without_disturbing_the_head() {
        let mut simulation = load_simulation();
        let resident = SimId(1);
        let first = PlayerTaskId(111);
        let second = PlayerTaskId(112);
        let far = TilePosition {
            floor: 0,
            x: 4,
            y: 1,
        };
        let home = TilePosition {
            floor: 0,
            x: 1,
            y: 1,
        };
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: first,
            resident,
            destination: far,
            priority: 0,
        });
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: second,
            resident,
            destination: home,
            priority: 0,
        });
        simulation.advance_tick();

        simulation.submit_player_command(PlayerCommand::CancelPlayerTask { task: second });
        simulation.advance_tick();

        // The second order is gone; the head continues to its destination.
        assert_eq!(
            resident_player_tasks(&simulation, resident),
            vec![ClientPlayerTaskSnapshot {
                id: first,
                state: ClientPlayerTaskState::Active,
            }]
        );
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToCancelled {
                    task: second,
                    resident,
                    destination: home,
                }
        }));

        for _ in 0..12 {
            simulation.advance_tick();
        }
        assert_eq!(simulation.resident_position(resident), Some(far));
        assert!(resident_player_tasks(&simulation, resident).is_empty());
        // The cancelled order never ran.
        assert!(!simulation.event_ledger().iter().any(|event| {
            matches!(
                event.kind,
                WorldEventKind::GoToArrived {
                    resident: r,
                    destination,
                } if r == resident && destination == home
            )
        }));
    }

    #[test]
    fn cancelling_the_active_head_dispatches_the_next_queued_task() {
        let mut simulation = load_simulation();
        let (toilet, affordance, slot) = toilet_ids();
        let resident = SimId(1);
        let use_task = PlayerTaskId(121);
        let move_task = PlayerTaskId(122);
        let far = TilePosition {
            floor: 0,
            x: 4,
            y: 1,
        };
        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task: use_task,
            resident,
            object: toilet.clone(),
            affordance: affordance.clone(),
            priority: 0,
        });
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: move_task,
            resident,
            destination: far,
            priority: 0,
        });
        simulation.advance_tick();
        assert_eq!(
            simulation.object_slot_claimant(&toilet, &slot),
            Some(resident)
        );

        simulation.submit_player_command(PlayerCommand::CancelPlayerTask { task: use_task });
        simulation.advance_tick();

        // The active use released and the queued move took over.
        assert_eq!(simulation.object_slot_claimant(&toilet, &slot), None);
        assert!(simulation.event_ledger().iter().any(|event| {
            matches!(
                &event.kind,
                WorldEventKind::TaskCancelled { task, .. } if *task == use_task
            )
        }));
        assert_eq!(
            resident_player_tasks(&simulation, resident),
            vec![ClientPlayerTaskSnapshot {
                id: move_task,
                state: ClientPlayerTaskState::Active,
            }]
        );
    }

    #[test]
    fn urgent_need_preempts_a_player_move_then_resumes_it() {
        let mut simulation = load_simulation();
        let (toilet, _, _) = toilet_ids();
        let resident = SimId(1);
        let move_task = PlayerTaskId(131);
        let far = TilePosition {
            floor: 0,
            x: 4,
            y: 1,
        };
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: move_task,
            resident,
            destination: far,
            priority: 0,
        });
        simulation.advance_tick();
        assert_eq!(
            simulation.resident_position(resident),
            Some(TilePosition {
                floor: 0,
                x: 2,
                y: 1,
            })
        );

        // Push the toilet need over its urgent threshold (authored 80).
        assert!(simulation.set_toilet_need(resident, 85));
        simulation.advance_tick();

        // The player move is paused, not cancelled, and the urgent walk to the
        // toilet has taken over.
        assert_eq!(
            resident_player_tasks(&simulation, resident),
            vec![ClientPlayerTaskSnapshot {
                id: move_task,
                state: ClientPlayerTaskState::Paused,
            }]
        );
        assert_eq!(
            simulation.resident_position(resident),
            Some(TilePosition {
                floor: 0,
                x: 3,
                y: 1,
            })
        );

        for _ in 0..12 {
            simulation.advance_tick();
        }

        // The urgent need was served and the paused move resumed to completion.
        assert_eq!(simulation.resident_position(resident), Some(far));
        assert!(resident_player_tasks(&simulation, resident).is_empty());
        assert!(simulation.event_ledger().iter().any(|event| {
            matches!(
                &event.kind,
                WorldEventKind::ObjectUseStarted { resident: r, object, .. }
                    if *r == resident && *object == toilet
            )
        }));
        assert!(simulation.event_ledger().iter().any(|event| {
            event.kind
                == WorldEventKind::GoToArrived {
                    resident,
                    destination: far,
                }
        }));
        // A paused order is never reported as cancelled.
        assert!(!simulation.event_ledger().iter().any(|event| {
            matches!(
                &event.kind,
                WorldEventKind::GoToCancelled { task, .. } if *task == move_task
            )
        }));
    }

    fn run_queue_and_preemption_script() -> Vec<WorldEvent> {
        let mut simulation = load_simulation();
        let (toilet, affordance, _) = toilet_ids();
        let resident = SimId(1);
        let far = TilePosition {
            floor: 0,
            x: 4,
            y: 1,
        };
        let home = TilePosition {
            floor: 0,
            x: 1,
            y: 1,
        };
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: PlayerTaskId(201),
            resident,
            destination: far,
            priority: 0,
        });
        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task: PlayerTaskId(202),
            resident,
            object: toilet,
            affordance,
            priority: 0,
        });
        simulation.submit_player_command(PlayerCommand::QueueGoTo {
            task: PlayerTaskId(203),
            resident,
            destination: home,
            priority: 0,
        });
        simulation.advance_tick();

        assert!(simulation.set_toilet_need(resident, 90));
        simulation.submit_player_command(PlayerCommand::CancelPlayerTask {
            task: PlayerTaskId(203),
        });
        for _ in 0..24 {
            simulation.advance_tick();
        }
        simulation.event_ledger().to_vec()
    }

    #[test]
    fn queue_and_preemption_script_is_deterministic() {
        let first = run_queue_and_preemption_script();
        let second = run_queue_and_preemption_script();
        assert_eq!(first, second, "queue and preemption remain repeatable");
    }
}
