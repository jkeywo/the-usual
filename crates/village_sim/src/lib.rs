//! Authoritative, deterministic village simulation.
//!
//! The first executable slice deliberately keeps the world small: it loads a
//! scenario and its people from RON, assigns stable runtime identifiers, and
//! advances an explicit fixed tick. Later slices add movement and actions to
//! the same scheduling contract.

use std::{collections::BTreeMap, fs, path::Path};

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
}

/// The content required to construct a scenario runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioContent {
    pub scenario: ScenarioDefinition,
    pub people: PeopleAsset,
}

impl ScenarioContent {
    /// Loads the initial domain-separated RON fixture from a content root.
    pub fn load_cottage_arrival(content_root: impl AsRef<Path>) -> Result<Self, ContentError> {
        let content_root = content_root.as_ref();
        let scenario = load_ron(content_root.join("scenarios/cottage_arrival.ron"))?;
        let people = load_ron(content_root.join("people/newcomers.ron"))?;
        Ok(Self { scenario, people })
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
}

/// The resident component retained on the authoritative ECS world.
#[derive(Component, Debug)]
struct Resident {
    definition_id: DefinitionId,
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

        let mut simulation = Self {
            world: World::new(),
            seed: content.scenario.seed,
            tick: 0,
            next_sim_id: 1,
            residents: BTreeMap::new(),
            pending_events: Vec::new(),
            ingested_events: Vec::new(),
            event_ledger: Vec::new(),
        };

        for definition_id in content.scenario.people {
            let person = definitions
                .get(&definition_id)
                .ok_or_else(|| ContentError::MissingPerson(definition_id.clone()))?;
            simulation.spawn_resident(person);
        }

        Ok(simulation)
    }

    fn spawn_resident(&mut self, person: &PersonDefinition) -> SimId {
        let sim_id = SimId(self.next_sim_id);
        self.next_sim_id += 1;
        let entity = self
            .world
            .spawn(Resident {
                definition_id: person.id.clone(),
            })
            .id();
        self.residents.insert(sim_id, entity);
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

    /// Consumes exactly one deterministic 250 ms simulation tick.
    ///
    /// Events published by a preceding tick first become observable here;
    /// this tick's own event is deferred until the next call.
    pub fn advance_tick(&mut self) {
        self.ingested_events = std::mem::take(&mut self.pending_events);
        self.tick += 1;

        let event = WorldEvent {
            tick: self.tick,
            kind: WorldEventKind::TickCompleted {
                keyed_marker: keyed_draw(self.seed, self.tick, self.tick, 0),
            },
        };
        self.event_ledger.push(event.clone());
        self.pending_events.push(event);
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
}
