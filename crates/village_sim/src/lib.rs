//! Authoritative, deterministic village simulation.
//!
//! The first executable slice deliberately keeps the world small: it loads a
//! scenario and its people from RON, assigns stable runtime identifiers, and
//! advances an explicit fixed tick. Later slices add movement and actions to
//! the same scheduling contract.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
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

/// The content required to construct a scenario runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioContent {
    pub scenario: ScenarioDefinition,
    pub people: PeopleAsset,
    pub map: MapAsset,
}

impl ScenarioContent {
    /// Loads the initial domain-separated RON fixture from a content root.
    pub fn load_cottage_arrival(content_root: impl AsRef<Path>) -> Result<Self, ContentError> {
        let content_root = content_root.as_ref();
        let scenario = load_ron(content_root.join("scenarios/cottage_arrival.ron"))?;
        let people = load_ron(content_root.join("people/newcomers.ron"))?;
        let map = load_ron(content_root.join("maps/cottage.ron"))?;
        Ok(Self {
            scenario,
            people,
            map,
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
}

#[derive(Clone, Debug)]
struct PortalTraversal {
    portal: DefinitionId,
    destination: TilePosition,
    ticks_remaining: u8,
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
    occupancy: BTreeMap<TilePosition, SimId>,
    portal_occupancy: BTreeMap<DefinitionId, SimId>,
    go_to: BTreeMap<SimId, GoToState>,
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

        let mut simulation = Self {
            world: World::new(),
            seed: content.scenario.seed,
            tick: 0,
            next_sim_id: 1,
            residents: BTreeMap::new(),
            map: content.map,
            occupancy: BTreeMap::new(),
            portal_occupancy: BTreeMap::new(),
            go_to: BTreeMap::new(),
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
            },
        );
        true
    }

    /// Consumes exactly one deterministic 250 ms simulation tick.
    ///
    /// Events published by a preceding tick first become observable here;
    /// this tick's own event is deferred until the next call.
    pub fn advance_tick(&mut self) {
        self.ingested_events = std::mem::take(&mut self.pending_events);
        self.tick += 1;

        self.tick_go_to();

        let event = WorldEvent {
            tick: self.tick,
            kind: WorldEventKind::TickCompleted {
                keyed_marker: keyed_draw(self.seed, self.tick, self.tick, 0),
            },
        };
        self.event_ledger.push(event.clone());
        self.pending_events.push(event);
    }

    fn tick_go_to(&mut self) {
        let residents = self.go_to.keys().copied().collect::<Vec<_>>();
        for resident in residents {
            self.tick_one_go_to(resident);
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
}
