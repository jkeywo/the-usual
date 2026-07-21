//! Authoritative, deterministic village simulation.
//!
//! The first executable slice deliberately keeps the world small: it loads a
//! scenario and its people from RON, assigns stable runtime identifiers, and
//! advances an explicit fixed tick. Later slices add movement and actions to
//! the same scheduling contract.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    fs,
    path::Path,
};

use bevy_ecs::{component::Component, entity::Entity, world::World};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The authoritative simulation interval. Presentation speed changes how
/// quickly ticks are consumed, never this value.
pub const TICK_DURATION_MS: u64 = 250;

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

/// The content required to construct a scenario runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioContent {
    pub scenario: ScenarioDefinition,
    pub people: PeopleAsset,
    pub map: MapAsset,
    pub objects: ObjectsAsset,
}

impl ScenarioContent {
    /// Loads the initial domain-separated RON fixture from a content root.
    pub fn load_cottage_arrival(content_root: impl AsRef<Path>) -> Result<Self, ContentError> {
        let content_root = content_root.as_ref();
        let scenario = load_ron(content_root.join("scenarios/cottage_arrival.ron"))?;
        let people = load_ron(content_root.join("people/newcomers.ron"))?;
        let map = load_ron(content_root.join("maps/cottage.ron"))?;
        let objects = load_ron(content_root.join("objects/cottage.ron"))?;
        Ok(Self {
            scenario,
            people,
            map,
            objects,
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
}

/// The resident component retained on the authoritative ECS world.
#[derive(Component, Debug)]
struct Resident {
    definition_id: DefinitionId,
}

#[derive(Component, Debug)]
struct Position(TilePosition);

#[derive(Clone, Debug)]
struct GoToState {
    destination: TilePosition,
    path: Vec<TilePosition>,
    next_step: usize,
    traversal: Option<PortalTraversal>,
    priority: i32,
    request_age: u64,
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

/// A typed order submitted by the presentation client. Commands enter the
/// authoritative inbox immediately but are only validated during the next
/// tick's ingest phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum PlayerCommand {
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
            Self::QueueUseToilet { task, .. } | Self::CancelPlayerTask { task } => *task,
        }
    }
}

/// Why a next-tick player command receipt was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum PlayerCommandRejection {
    DuplicateTask,
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
    Completed,
    Cancelled,
}

fn compare_use_requests(left: &UseRequest, right: &UseRequest) -> std::cmp::Ordering {
    right
        .priority
        .cmp(&left.priority)
        .then_with(|| left.request_age.cmp(&right.request_age))
        .then_with(|| left.resident.cmp(&right.resident))
}

fn compare_movement_claims(left: &MovementClaim, right: &MovementClaim) -> std::cmp::Ordering {
    right
        .priority
        .cmp(&left.priority)
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
    next_sim_id: u64,
    residents: BTreeMap<SimId, Entity>,
    map: MapAsset,
    objects: BTreeMap<DefinitionId, SmartObjectDefinition>,
    occupancy: BTreeMap<TilePosition, SimId>,
    portal_occupancy: BTreeMap<DefinitionId, SimId>,
    go_to: BTreeMap<SimId, GoToState>,
    use_requests: Vec<UseRequest>,
    active_object_uses: BTreeMap<SimId, ActiveObjectUse>,
    player_command_inbox: Vec<PlayerCommand>,
    player_tasks: BTreeMap<PlayerTaskId, PlayerTaskState>,
    object_slot_claims: BTreeMap<(DefinitionId, DefinitionId), SimId>,
    capability_claims: BTreeMap<(SimId, Capability), DefinitionId>,
    pending_events: Vec<WorldEvent>,
    ingested_events: Vec<WorldEvent>,
    event_ledger: Vec<WorldEvent>,
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

        let mut simulation = Self {
            world: World::new(),
            seed: content.scenario.seed,
            tick: 0,
            next_sim_id: 1,
            residents: BTreeMap::new(),
            map: content.map,
            objects,
            occupancy: BTreeMap::new(),
            portal_occupancy: BTreeMap::new(),
            go_to: BTreeMap::new(),
            use_requests: Vec::new(),
            active_object_uses: BTreeMap::new(),
            player_command_inbox: Vec::new(),
            player_tasks: BTreeMap::new(),
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
            simulation.spawn_resident(person, placement.position);
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
                request_age: self.tick,
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
            request_age: self.tick,
            player_task: None,
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

    /// Consumes exactly one deterministic 250 ms simulation tick.
    ///
    /// Events published by a preceding tick first become observable here;
    /// this tick's own event is deferred until the next call.
    pub fn advance_tick(&mut self) {
        self.ingested_events = std::mem::take(&mut self.pending_events);
        self.tick += 1;

        self.ingest_player_commands();
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

    fn ingest_player_commands(&mut self) {
        let commands = std::mem::take(&mut self.player_command_inbox);
        for command in commands {
            match command {
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
        if self.active_object_uses.contains_key(&resident)
            || self
                .use_requests
                .iter()
                .any(|request| request.resident == resident)
        {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::ResidentBusy,
            });
            return;
        }
        let is_valid_target = self.objects.get(&object).is_some_and(|definition| {
            definition
                .affordances
                .iter()
                .any(|candidate| candidate.id == affordance)
        });
        if !is_valid_target {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::InvalidUseTarget,
            });
            return;
        }

        self.player_tasks.insert(task, PlayerTaskState::Queued);
        self.use_requests.push(UseRequest {
            resident,
            object,
            affordance,
            priority,
            request_age: self.tick,
            player_task: Some(task),
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
        if !matches!(state, PlayerTaskState::Queued | PlayerTaskState::Active) {
            self.emit(WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::TaskNotCancellable,
            });
            return;
        }

        self.emit(WorldEventKind::PlayerCommandAccepted { task });
        if state == PlayerTaskState::Queued {
            let request = self
                .use_requests
                .iter()
                .position(|request| request.player_task == Some(task))
                .map(|index| self.use_requests.remove(index))
                .expect("queued player task always has a use request");
            self.player_tasks.insert(task, PlayerTaskState::Cancelled);
            self.emit(WorldEventKind::TaskCancelled {
                task,
                resident: request.resident,
                object: request.object,
                affordance: request.affordance,
            });
            return;
        }

        let (resident, active) = self
            .active_object_uses
            .iter()
            .find(|(_, active)| active.player_task == Some(task))
            .map(|(resident, active)| (*resident, active.clone()))
            .expect("active player task always has an active object use");
        self.active_object_uses.remove(&resident);
        self.object_slot_claims
            .remove(&(active.object.clone(), active.slot.clone()));
        self.capability_claims
            .remove(&(resident, active.capability));
        self.player_tasks.insert(task, PlayerTaskState::Cancelled);
        self.emit(WorldEventKind::TaskCancelled {
            task,
            resident,
            object: active.object,
            affordance: active.affordance,
        });
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
                self.object_slot_claims
                    .remove(&(active.object.clone(), active.slot.clone()));
                self.capability_claims
                    .remove(&(resident, active.capability));
                if let Some(task) = active.player_task {
                    self.player_tasks.insert(task, PlayerTaskState::Completed);
                }
                self.emit(WorldEventKind::ObjectUseCompleted {
                    resident,
                    object: active.object,
                    affordance: active.affordance,
                });
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
            self.emit(WorldEventKind::GoToFailed {
                resident,
                destination: state.destination,
            });
            return;
        }
        if origin == state.destination {
            self.emit(WorldEventKind::GoToArrived {
                resident,
                destination: state.destination,
            });
            return;
        }

        let Some(next) = state.path.get(state.next_step).copied() else {
            self.emit(WorldEventKind::GoToFailed {
                resident,
                destination: state.destination,
            });
            return;
        };
        if let Some(blocked_by) = self.occupancy.get(&next).copied() {
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

    fn heuristic(&self, from: TilePosition, to: TilePosition) -> u32 {
        from.x.abs_diff(to.x) + from.y.abs_diff(to.y) + u32::from(from.floor != to.floor)
    }
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
}
