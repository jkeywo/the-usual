//! Read-only Bevy presentation for the Cottage Contention fixture.

use std::collections::BTreeMap;

use bevy::prelude::*;
use bevy::{asset::AssetPlugin, input::mouse::MouseWheel};
use village_sim::{
    ClientIntention, ClientPerception, ClientPlayerTaskState, CottageSnapshot, DefinitionId,
    PlayerCommand, PlayerCommandRejection, PlayerTaskId, ScenarioContent, SimId, Simulation,
    TilePosition, TimeOfDay, WorldEvent, WorldEventKind,
};

/// The authored cottage remains a 32px grid. Residents and furniture use a
/// deliberately larger presentation scale so they read clearly within it.
const TILE_PIXELS: f32 = 32.0;
const ACTOR_AND_FURNITURE_SCALE: f32 = 2.0;
const CHARACTER_COLUMNS: u32 = 8;
const CLOTHING_FIRST_INDEX: usize = 32;
/// The number of listed player tasks the card exposes an individual cancel
/// control for. A resident's queue is not expected to grow past this in the
/// Cottage fixture; deeper queues simply show their first few cancel buttons.
const CANCEL_SLOTS: usize = 4;

#[derive(Resource)]
struct SimulationDriver {
    simulation: Simulation,
    previous: CottageSnapshot,
    current: CottageSnapshot,
    tick_timer: Timer,
}

/// Selection is presentation-only. The simulation remains unaware of it.
#[derive(Default, Resource)]
struct SelectedResident(Option<SimId>);

/// All camera state is a client resource.  Neither following nor floor focus
/// is represented in, or fed back to, the deterministic simulation.
#[derive(Resource)]
struct CottageCamera {
    follow_selected: bool,
    focused_floor: u8,
    zoom: u8,
}

#[derive(Default, Resource)]
struct PanDrag(Option<Vec2>);

impl Default for CottageCamera {
    fn default() -> Self {
        Self {
            follow_selected: false,
            focused_floor: 0,
            // Start wide enough to read the cottage as a room rather than a
            // wall-to-wall field of 32px tiles.
            zoom: 1,
        }
    }
}

#[derive(Component)]
struct CottageCameraEntity;

/// A world sprite that belongs to one cottage storey. UI deliberately never
/// receives this component.
#[derive(Component)]
struct FloorVisual(u8);

#[derive(Component)]
struct ResidentSelector(SimId);

#[derive(Component)]
struct StatusCardText;

/// The in-world day clock shown as a small always-on HUD readout.
#[derive(Component)]
struct ClockText;

#[derive(Component)]
struct OrderFeedbackText;

#[derive(Component)]
struct UseToiletButton;

/// One of a fixed pool of per-task cancel controls. Its `index` addresses the
/// selected resident's player task list, head first.
#[derive(Component)]
struct CancelTaskSlot {
    index: usize,
}

#[derive(Component)]
struct CancelSlotText {
    index: usize,
}

/// The task (and its snapshot state at bind time) each cancel slot currently
/// targets. Rebuilt every frame from the selected resident's projected queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CancelBinding {
    task: PlayerTaskId,
    outcome: CancellationOutcome,
}

#[derive(Default, Resource)]
struct CancelBindings([Option<CancelBinding>; CANCEL_SLOTS]);

#[derive(Component)]
struct SemanticEventFeedText;

/// Allocates task IDs in the client only. An allocated ID is retained by the
/// pending order until the simulation publishes its immutable receipt.
#[derive(Default, Resource)]
struct OrderState {
    next_task_id: u64,
    pending: Option<PendingOrder>,
    receipt: Option<String>,
    /// Cancellation outcomes must be remembered after the deferred command
    /// receipt has cleared `pending`: `TaskCancelled` is the semantic event
    /// which tells the player what was actually released.
    cancellation_outcomes: BTreeMap<PlayerTaskId, CancellationOutcome>,
}

#[derive(Clone, Copy)]
struct PendingOrder {
    task: PlayerTaskId,
    action: PendingAction,
    /// The immutable ledger length at submission. A cancellation shares its
    /// task ID with its already accepted order, so only later events can be
    /// its own deferred receipt.
    receipt_start: usize,
}

#[derive(Clone, Copy)]
enum PendingAction {
    Order,
    Cancellation(CancellationOutcome),
}

/// The authoritative task state at the instant the player asked to cancel.
/// It is presentation context only; the cancel command still goes through the
/// simulation and is validated there on its next tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CancellationOutcome {
    Queued,
    Active,
    Paused,
}

/// Cursor for the append-only immutable event ledger. This keeps display
/// messages one-time while preserving the ledger for simulation assertions.
#[derive(Default, Resource)]
struct SemanticEventFeed {
    consumed_events: usize,
    entries: Vec<String>,
}

/// This state deliberately does not share the simulation seed. UI pitch is
/// presentation-only variation, never an input to the deterministic world.
#[derive(Resource)]
struct UiAudioVariation(u64);

#[derive(Component)]
enum CameraControlButton {
    Follow,
    Floor(u8),
}

#[derive(Component)]
struct FollowButtonText;

/// A client-only ground highlight for the currently inspected newcomer.
#[derive(Component)]
struct SelectedResidentMarker;

#[derive(Clone, Copy, Component)]
struct ResidentVisual {
    id: village_sim::SimId,
}

#[derive(Component)]
struct WalkFrames {
    first_index: usize,
}

/// Marks the transparent clothing layer for a one-time, client-only texture
/// bake.  Keeping this separate from `Sprite::color` is important: tinting
/// multiplies every source colour and loses the authored highlights/shadows.
#[derive(Component)]
struct ClothingHue {
    degrees: f32,
}

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(ImagePlugin::default_nearest())
                .set(AssetPlugin {
                    file_path: asset_root(),
                    ..default()
                }),
        )
        .add_systems(Startup, setup_cottage)
        .add_systems(
            Update,
            (
                (
                    bake_clothing_hues,
                    advance_simulation,
                    interpolate_residents,
                    animate_walking,
                    select_resident_from_sprite,
                    select_resident_from_card,
                    queue_selected_go_to,
                    submit_selected_toilet_order,
                    submit_selected_task_cancellation,
                )
                    .chain(),
                (
                    update_camera_controls,
                    update_camera_control_labels,
                    pan_and_zoom_camera,
                    follow_selected_resident,
                    update_resident_floor_layers,
                    update_selected_resident_marker,
                    apply_floor_focus,
                )
                    .chain(),
                (
                    update_clock_text,
                    update_status_card,
                    update_order_feedback,
                    update_order_feedback_text,
                    correct_pending_action_feedback_text,
                    update_cancel_slots,
                    update_semantic_event_feed,
                    update_semantic_event_feed_text,
                )
                    .chain(),
            )
                .chain(),
        )
        .run();
}

fn setup_cottage(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut layouts: ResMut<Assets<TextureAtlasLayout>>,
) {
    let simulation = Simulation::from_content(cottage_content()).expect("Cottage content resolves");
    let snapshot = simulation.cottage_snapshot();
    commands.insert_resource(SimulationDriver {
        simulation,
        previous: snapshot.clone(),
        current: snapshot.clone(),
        tick_timer: Timer::from_seconds(
            village_sim::TICK_DURATION_MS as f32 / 1000.0,
            TimerMode::Repeating,
        ),
    });
    commands.insert_resource(SelectedResident::default());
    commands.insert_resource(CottageCamera::default());
    commands.insert_resource(PanDrag::default());
    commands.insert_resource(OrderState {
        next_task_id: 1,
        ..default()
    });
    commands.insert_resource(SemanticEventFeed::default());
    commands.insert_resource(CancelBindings::default());
    commands.insert_resource(UiAudioVariation(client_audio_seed()));
    // The authored cottage occupies positive tile coordinates. Centre the
    // initial ground-floor view on its bounds rather than the empty world
    // origin, which otherwise exposes only Bevy's clear colour.
    let starting_centre = snapshot
        .residents
        .iter()
        .map(|resident| {
            tile_to_world(resident.position.x, resident.position.y)
                + floor_offset(resident.position.floor)
        })
        .reduce(|total, position| total + position)
        .map_or(Vec2::ZERO, |total| total / snapshot.residents.len() as f32);
    commands.spawn((
        Camera2d,
        Transform::from_xyz(starting_centre.x, starting_centre.y, 0.0),
        CottageCameraEntity,
    ));
    commands.spawn((
        Sprite::from_color(
            Color::srgba(0.95, 0.8, 0.2, 0.45),
            Vec2::splat(TILE_PIXELS + 4.0),
        ),
        Transform::from_xyz(0.0, 0.0, 3.0),
        Visibility::Hidden,
        SelectedResidentMarker,
        FloorVisual(0),
    ));
    let house = asset_server.load("client/tiles/house_tiles.png");
    let house_layout = layouts.add(TextureAtlasLayout::from_grid(
        UVec2::splat(32),
        10,
        10,
        None,
        None,
    ));
    spawn_floor(
        &mut commands,
        &snapshot,
        0,
        Vec2::ZERO,
        house.clone(),
        house_layout.clone(),
    );
    spawn_floor(
        &mut commands,
        &snapshot,
        1,
        Vec2::new(0.0, 28.0 * TILE_PIXELS),
        house,
        house_layout,
    );

    let character = asset_server.load("client/characters/global.png");
    for (index, resident) in snapshot.residents.iter().enumerate() {
        let floor_offset = if resident.position.floor == 0 {
            0.0
        } else {
            28.0 * TILE_PIXELS
        };
        let position =
            tile_to_world(resident.position.x, resident.position.y) + Vec2::new(0.0, floor_offset);
        // These are hue *rotations*, rather than colour tints. The CPU bake
        // below leaves each pixel's value and alpha intact, so the source
        // clothing's folds and highlights remain visible.
        let clothing_hue = if index == 0 { 24.0 } else { 202.0 };
        let layout = layouts.add(TextureAtlasLayout::from_grid(
            UVec2::splat(32),
            8,
            12,
            None,
            None,
        ));
        commands.spawn((
            Sprite::from_atlas_image(
                character.clone(),
                TextureAtlas {
                    layout: layout.clone(),
                    index: 0,
                },
            ),
            Transform::from_xyz(position.x, position.y, 4.0)
                .with_scale(Vec3::splat(ACTOR_AND_FURNITURE_SCALE)),
            Name::new(resident.display_name.clone()),
            ResidentVisual { id: resident.id },
            WalkFrames { first_index: 0 },
            FloorVisual(resident.position.floor),
        ));
        // The clothing is a separate transparent atlas row. It gets a private
        // hue-rotated image at runtime; the body sprite always keeps the
        // authored source image.
        commands.spawn((
            Sprite::from_atlas_image(
                character.clone(),
                TextureAtlas {
                    layout,
                    index: CLOTHING_FIRST_INDEX,
                },
            ),
            Transform::from_xyz(position.x, position.y, 5.0)
                .with_scale(Vec3::splat(ACTOR_AND_FURNITURE_SCALE)),
            ResidentVisual { id: resident.id },
            WalkFrames {
                first_index: CLOTHING_FIRST_INDEX,
            },
            ClothingHue {
                degrees: clothing_hue,
            },
            FloorVisual(resident.position.floor),
        ));
    }

    let furniture = asset_server.load("client/tiles/furniture.png");
    let furniture_layout = layouts.add(TextureAtlasLayout::from_grid(
        UVec2::splat(32),
        12,
        15,
        None,
        None,
    ));
    for object in &snapshot.objects {
        let position = tile_to_world(object.position.x, object.position.y)
            + floor_offset(object.position.floor);
        commands.spawn((
            Sprite::from_atlas_image(
                furniture.clone(),
                TextureAtlas {
                    layout: furniture_layout.clone(),
                    index: furniture_atlas_index(&object.object_type),
                },
            ),
            Transform::from_xyz(position.x, position.y, 2.0)
                .with_scale(Vec3::splat(ACTOR_AND_FURNITURE_SCALE)),
            Name::new(object.id.0.clone()),
            FloorVisual(object.position.floor),
        ));
    }

    spawn_status_card(&mut commands, &asset_server, &snapshot);

    let font = asset_server.load("client/ui/kenney_future_narrow.ttf");
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(16.0),
                top: Val::Px(16.0),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.09, 0.16, 0.85)),
        ))
        .with_child((
            Text::new(format_clock(snapshot.time_of_day)),
            TextFont {
                font,
                font_size: 24.0,
                ..default()
            },
            TextColor(Color::WHITE),
            ClockText,
        ));
}

fn format_clock(time: TimeOfDay) -> String {
    format!("{:02}:{:02}", time.hour, time.minute)
}

/// Maps an authored object type to a placeholder furniture atlas cell. Cell
/// 144 is the toilet; the King's Head bar uses a distinct cell until finished
/// art arrives, so it never masquerades as another fixture.
fn furniture_atlas_index(object_type: &DefinitionId) -> usize {
    match object_type.0.as_str() {
        "object_type.bar" => 108,
        _ => 144,
    }
}

fn spawn_status_card(
    commands: &mut Commands,
    asset_server: &AssetServer,
    snapshot: &CottageSnapshot,
) {
    let font = asset_server.load("client/ui/kenney_future_narrow.ttf");
    let button = asset_server.load("client/ui/button.png");
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(16.0),
                bottom: Val::Px(16.0),
                width: Val::Px(300.0),
                padding: UiRect::all(Val::Px(14.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.06, 0.09, 0.16)),
            BorderColor::all(Color::srgb(0.38, 0.63, 0.86)),
        ))
        .with_children(|card| {
            card.spawn((
                Text::new("Select a newcomer to inspect them."),
                TextFont {
                    font: font.clone(),
                    font_size: 22.0,
                    ..default()
                },
                TextColor(Color::WHITE),
                StatusCardText,
            ));
            for resident in &snapshot.residents {
                card.spawn((
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(38.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ImageNode::new(button.clone()),
                    ResidentSelector(resident.id),
                ))
                .with_child((
                    Text::new(resident.display_name.clone()),
                    TextFont {
                        font: font.clone(),
                        font_size: 18.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.04, 0.08, 0.12)),
                ));
            }
            card.spawn((
                Button,
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(38.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                ImageNode::new(button.clone()),
                UseToiletButton,
            ))
            .with_child((
                Text::new("Use toilet"),
                TextFont {
                    font: font.clone(),
                    font_size: 18.0,
                    ..default()
                },
                TextColor(Color::srgb(0.04, 0.08, 0.12)),
            ));
            card.spawn((
                Text::new(""),
                TextFont {
                    font: font.clone(),
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.9, 1.0)),
                OrderFeedbackText,
            ));
            // One cancel control per listed task. Each starts hidden and
            // becomes available only while the selected resident's queue has a
            // task at that position.
            for index in 0..CANCEL_SLOTS {
                card.spawn((
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(34.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ImageNode::new(button.clone()),
                    Visibility::Hidden,
                    CancelTaskSlot { index },
                ))
                .with_child((
                    Text::new("Cancel task"),
                    TextFont {
                        font: font.clone(),
                        font_size: 16.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.04, 0.08, 0.12)),
                    CancelSlotText { index },
                ));
            }
            card.spawn((
                Text::new(""),
                TextFont {
                    font: font.clone(),
                    font_size: 15.0,
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.9, 1.0)),
                SemanticEventFeedText,
            ));
            card.spawn((
                Button,
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(32.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                ImageNode::new(button.clone()),
                CameraControlButton::Follow,
            ))
            .with_child((
                Text::new("Follow selected: off"),
                TextFont {
                    font: font.clone(),
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(0.04, 0.08, 0.12)),
                FollowButtonText,
            ));
            for (floor, label) in [(0, "Ground floor"), (1, "Upstairs")] {
                card.spawn((
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(28.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ImageNode::new(button.clone()),
                    CameraControlButton::Floor(floor),
                ))
                .with_child((
                    Text::new(label),
                    TextFont {
                        font: font.clone(),
                        font_size: 15.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.04, 0.08, 0.12)),
                ));
            }
        });
}

/// The Cottage fixture contains one authored toilet. This control is
/// agent-first: it never turns the object into a client-side click target.
fn submit_selected_toilet_order(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut buttons: Query<&Interaction, (With<UseToiletButton>, Changed<Interaction>)>,
    selected: Res<SelectedResident>,
    mut driver: ResMut<SimulationDriver>,
    mut order: ResMut<OrderState>,
    mut audio_variation: ResMut<UiAudioVariation>,
) {
    if !buttons
        .iter_mut()
        .any(|interaction| *interaction == Interaction::Pressed)
    {
        return;
    }
    let Some(resident) = selected.0 else {
        order.receipt = Some("Select a newcomer before ordering.".to_owned());
        return;
    };
    if order.pending.is_some() {
        order.receipt = Some("An order is already pending; wait for its receipt.".to_owned());
        return;
    }

    let task = PlayerTaskId(order.next_task_id);
    order.next_task_id += 1;
    driver
        .simulation
        .submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident,
            object: DefinitionId::new("object.cottage_toilet"),
            affordance: DefinitionId::new("affordance.use_toilet"),
            priority: 100,
        });
    order.pending = Some(PendingOrder {
        task,
        action: PendingAction::Order,
        receipt_start: driver.simulation.event_ledger().len(),
    });
    order.receipt = None;
    play_ui_click(&mut commands, &asset_server, &mut audio_variation);
}

/// Cancels only work that the immutable snapshot identifies as queued or
/// active for the selected resident.
fn submit_selected_task_cancellation(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    slots: Query<(&Interaction, &CancelTaskSlot), Changed<Interaction>>,
    bindings: Res<CancelBindings>,
    mut driver: ResMut<SimulationDriver>,
    mut order: ResMut<OrderState>,
    mut audio_variation: ResMut<UiAudioVariation>,
) {
    if order.pending.is_some() {
        return;
    }
    let Some(binding) = slots.iter().find_map(|(interaction, slot)| {
        (*interaction == Interaction::Pressed)
            .then(|| bindings.0.get(slot.index).copied().flatten())
            .flatten()
    }) else {
        return;
    };
    driver
        .simulation
        .submit_player_command(PlayerCommand::CancelPlayerTask { task: binding.task });
    order.pending = Some(PendingOrder {
        task: binding.task,
        action: PendingAction::Cancellation(binding.outcome),
        receipt_start: driver.simulation.event_ledger().len(),
    });
    order
        .cancellation_outcomes
        .insert(binding.task, binding.outcome);
    order.receipt = None;
    play_ui_click(&mut commands, &asset_server, &mut audio_variation);
}

/// Projects the selected resident's cancellable player tasks, head first, each
/// paired with the snapshot state that decides its cancellation wording.
fn cancellable_tasks(selected: Option<SimId>, snapshot: &CottageSnapshot) -> Vec<CancelBinding> {
    selected
        .and_then(|id| snapshot.residents.iter().find(|resident| resident.id == id))
        .map(|resident| {
            resident
                .player_tasks
                .iter()
                .map(|task| CancelBinding {
                    task: task.id,
                    outcome: match task.state {
                        ClientPlayerTaskState::Queued => CancellationOutcome::Queued,
                        ClientPlayerTaskState::Active => CancellationOutcome::Active,
                        ClientPlayerTaskState::Paused => CancellationOutcome::Paused,
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Consumes an immutable receipt exactly once from the simulation's published
/// ledger. The ledger records the receipt in the same advance that validates
/// the command, while simulation systems still observe it next tick through
/// their deferred event path. Receipt handling never changes simulation state.
fn update_order_feedback(mut order: ResMut<OrderState>, driver: Res<SimulationDriver>) {
    consume_order_receipt(&mut order, driver.simulation.event_ledger());
}

fn consume_order_receipt(order: &mut OrderState, events: &[WorldEvent]) {
    let Some(pending) = order.pending else {
        return;
    };
    let Some(receipt) = deferred_receipt(pending, events) else {
        return;
    };
    order.pending = None;
    order.receipt = Some(receipt);
}

fn deferred_receipt(pending: PendingOrder, events: &[WorldEvent]) -> Option<String> {
    events
        .iter()
        .skip(pending.receipt_start)
        .find_map(|event| match &event.kind {
            WorldEventKind::PlayerCommandAccepted { task } if *task == pending.task => {
                Some(format!(
                    "{} #{} accepted.",
                    pending_action_label(pending.action),
                    pending.task.0
                ))
            }
            WorldEventKind::PlayerCommandRejected {
                task: received,
                reason,
            } if *received == pending.task => Some(format!(
                "{} #{} rejected: {reason}.",
                pending_action_label(pending.action),
                pending.task.0,
                reason = rejection_label(*reason)
            )),
            _ => None,
        })
}

fn pending_action_label(action: PendingAction) -> &'static str {
    match action {
        PendingAction::Order => "Order",
        PendingAction::Cancellation(_) => "Cancellation",
    }
}

fn rejection_label(reason: PlayerCommandRejection) -> &'static str {
    match reason {
        PlayerCommandRejection::DuplicateTask => "duplicate task",
        PlayerCommandRejection::InvalidMoveTarget => "unreachable tile",
        PlayerCommandRejection::InvalidUseTarget => "invalid fixture",
        PlayerCommandRejection::ResidentBusy => "resident busy",
        PlayerCommandRejection::UnknownResident => "unknown resident",
        PlayerCommandRejection::UnknownTask => "unknown task",
        PlayerCommandRejection::TaskNotCancellable => "task cannot be cancelled",
    }
}

fn update_order_feedback_text(
    order: Res<OrderState>,
    mut text: Query<&mut Text, With<OrderFeedbackText>>,
) {
    if !order.is_changed() {
        return;
    }
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    text.0 = order.pending.map_or_else(
        || order.receipt.clone().unwrap_or_default(),
        |pending| format!("Order #{} pending…", pending.task.0),
    );
}

fn update_cancel_slots(
    selected: Res<SelectedResident>,
    driver: Res<SimulationDriver>,
    order: Res<OrderState>,
    mut bindings: ResMut<CancelBindings>,
    mut slots: Query<(&CancelTaskSlot, &mut Visibility)>,
    mut labels: Query<(&CancelSlotText, &mut Text)>,
) {
    if !selected.is_changed() && !driver.is_changed() && !order.is_changed() {
        return;
    }
    // A cancel is only offered while no command is already in flight.
    let tasks = if order.pending.is_some() {
        Vec::new()
    } else {
        cancellable_tasks(selected.0, &driver.current)
    };
    bindings.0 = std::array::from_fn(|index| tasks.get(index).copied());
    for (slot, mut visibility) in &mut slots {
        *visibility = if bindings.0.get(slot.index).copied().flatten().is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for (label, mut text) in &mut labels {
        text.0 = bindings.0.get(label.index).copied().flatten().map_or_else(
            || "Cancel task".to_owned(),
            |binding| {
                format!(
                    "Cancel #{} {}",
                    binding.task.0,
                    cancel_state_word(binding.outcome)
                )
            },
        );
    }
}

fn cancel_state_word(outcome: CancellationOutcome) -> &'static str {
    match outcome {
        CancellationOutcome::Queued => "queued",
        CancellationOutcome::Active => "active",
        CancellationOutcome::Paused => "paused",
    }
}

/// `update_order_feedback_text` owns the established order copy. Override
/// only the in-flight cancellation wording so the same task ID is not
/// misleadingly presented as a second order.
fn correct_pending_action_feedback_text(
    order: Res<OrderState>,
    mut text: Query<&mut Text, With<OrderFeedbackText>>,
) {
    let Some(PendingOrder {
        task,
        action: PendingAction::Cancellation(outcome),
        ..
    }) = order.pending
    else {
        return;
    };
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    text.0 = match outcome {
        CancellationOutcome::Queued => format!("Removing queued order #{}...", task.0),
        CancellationOutcome::Active => format!("Cancelling active order #{}...", task.0),
        CancellationOutcome::Paused => format!("Removing paused order #{}...", task.0),
    };
}

fn update_semantic_event_feed(
    mut feed: ResMut<SemanticEventFeed>,
    mut order: ResMut<OrderState>,
    driver: Res<SimulationDriver>,
) {
    consume_semantic_events(
        &mut feed,
        driver.simulation.event_ledger(),
        &driver.current,
        &mut order.cancellation_outcomes,
    );
}

fn consume_semantic_events(
    feed: &mut SemanticEventFeed,
    events: &[WorldEvent],
    snapshot: &CottageSnapshot,
    cancellation_outcomes: &mut BTreeMap<PlayerTaskId, CancellationOutcome>,
) {
    for event in events.iter().skip(feed.consumed_events) {
        if let Some(entry) = semantic_event_label(&event.kind, snapshot, cancellation_outcomes) {
            feed.entries.push(entry);
        }
    }
    feed.consumed_events = events.len();
    const VISIBLE_EVENTS: usize = 4;
    if feed.entries.len() > VISIBLE_EVENTS {
        feed.entries.drain(..feed.entries.len() - VISIBLE_EVENTS);
    }
}

fn semantic_event_label(
    kind: &WorldEventKind,
    snapshot: &CottageSnapshot,
    cancellation_outcomes: &mut BTreeMap<PlayerTaskId, CancellationOutcome>,
) -> Option<String> {
    let resident_name = |id: SimId| {
        snapshot
            .residents
            .iter()
            .find(|resident| resident.id == id)
            .map_or("A newcomer", |resident| resident.display_name.as_str())
    };
    match kind {
        WorldEventKind::ObjectUseWaited { resident, .. } => Some(format!(
            "{} is waiting for the toilet.",
            resident_name(*resident)
        )),
        WorldEventKind::ObjectUseStarted { resident, .. } => Some(format!(
            "{} started using the toilet.",
            resident_name(*resident)
        )),
        WorldEventKind::ObjectUseCompleted { resident, .. } => Some(format!(
            "{} finished using the toilet.",
            resident_name(*resident)
        )),
        WorldEventKind::TaskCancelled { task, resident, .. } => {
            let outcome = cancellation_outcomes.remove(task)?;
            let outcome_label = match outcome {
                CancellationOutcome::Queued => "removed from queue",
                CancellationOutcome::Active => "toilet released",
                CancellationOutcome::Paused => "removed from queue",
            };
            Some(format!(
                "Order #{} for {} cancelled; {outcome_label}.",
                task.0,
                resident_name(*resident)
            ))
        }
        WorldEventKind::NeighbourInvitation { event_at, .. } => Some(format!(
            "A neighbour invited the household to the {} quiz.",
            format_clock(*event_at)
        )),
        WorldEventKind::QuizArrived { resident } => Some(format!(
            "{} arrived at the King's Head for the quiz.",
            resident_name(*resident)
        )),
        WorldEventKind::PargeterSeatTaken {
            participant,
            witnessed_by,
        } => {
            let reaction = witnessed_by.first().map_or_else(
                || String::from("No one seems to have noticed."),
                |witness| format!("{} tuts.", resident_name(*witness)),
            );
            Some(format!(
                "{} took Mr Pargeter's corner seat. {reaction} The household now know it is his.",
                resident_name(*participant)
            ))
        }
        _ => None,
    }
}

fn update_semantic_event_feed_text(
    feed: Res<SemanticEventFeed>,
    mut text: Query<&mut Text, With<SemanticEventFeedText>>,
) {
    if !feed.is_changed() {
        return;
    }
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    text.0 = feed.entries.join("\n");
}

#[cfg(not(target_arch = "wasm32"))]
fn client_audio_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64)
}

// `std::time::SystemTime` panics on `wasm32-unknown-unknown` ("time not
// implemented on this platform"). The UI pitch variation is cosmetic and never
// feeds the simulation, so a fixed seed is acceptable on the web.
#[cfg(target_arch = "wasm32")]
fn client_audio_seed() -> u64 {
    0x9E37_79B9_7F4A_7C15
}

fn next_ui_pitch(variation: &mut UiAudioVariation) -> f32 {
    variation.0 = variation
        .0
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1);
    let unit = ((variation.0 >> 40) as f32) / ((1_u32 << 24) as f32 - 1.0);
    0.94 + unit * 0.12
}

fn play_ui_click(
    commands: &mut Commands,
    asset_server: &AssetServer,
    variation: &mut UiAudioVariation,
) {
    let sound = if variation.0 & 1 == 0 {
        "client/audio/click-a.ogg"
    } else {
        "client/audio/tap-a.ogg"
    };
    commands.spawn((
        AudioPlayer::new(asset_server.load(sound)),
        PlaybackSettings::DESPAWN.with_speed(next_ui_pitch(variation)),
    ));
}

fn select_resident_from_card(
    mut selected: ResMut<SelectedResident>,
    selectors: Query<(&Interaction, &ResidentSelector), Changed<Interaction>>,
) {
    for (interaction, selector) in &selectors {
        if *interaction == Interaction::Pressed {
            selected.0 = Some(selector.0);
        }
    }
}

fn update_camera_controls(
    mut controls: ResMut<CottageCamera>,
    buttons: Query<(&Interaction, &CameraControlButton), Changed<Interaction>>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *button {
            CameraControlButton::Follow => controls.follow_selected = !controls.follow_selected,
            CameraControlButton::Floor(floor) => {
                controls.focused_floor = floor;
                // A manual floor choice is an explicit request to explore,
                // rather than to remain locked onto the selected newcomer.
                controls.follow_selected = false;
            }
        }
    }
}

fn update_camera_control_labels(
    controls: Res<CottageCamera>,
    mut labels: Query<&mut Text, With<FollowButtonText>>,
) {
    if !controls.is_changed() {
        return;
    }
    for mut label in &mut labels {
        label.0 = format!(
            "Follow selected: {}",
            if controls.follow_selected {
                "on"
            } else {
                "off"
            }
        );
    }
}

/// Right-drag pans the view. The wheel moves through only integral display
/// magnifications (1x–4x), so a source pixel is never filtered into a partial
/// display pixel.
fn pan_and_zoom_camera(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    mut wheel: MessageReader<MouseWheel>,
    mut drag: ResMut<PanDrag>,
    mut controls: ResMut<CottageCamera>,
    mut camera: Query<(&mut Transform, &mut Projection), With<CottageCameraEntity>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((mut transform, mut projection)) = camera.single_mut() else {
        return;
    };

    let scroll: f32 = wheel.read().map(|event| event.y).sum();
    if scroll != 0.0 {
        controls.zoom = bounded_zoom(controls.zoom, scroll);
    }
    if let Projection::Orthographic(orthographic) = &mut *projection {
        orthographic.scale = 1.0 / f32::from(controls.zoom);
    }

    let cursor = window.cursor_position();
    if mouse.just_pressed(MouseButton::Right) {
        drag.0 = cursor;
    }
    if mouse.pressed(MouseButton::Right)
        && let (Some(previous), Some(current)) = (drag.0, cursor)
    {
        let delta = current - previous;
        // Projection scale maps viewport pixels into world pixels here.
        transform.translation.x =
            (transform.translation.x - delta.x / f32::from(controls.zoom)).round();
        transform.translation.y =
            (transform.translation.y + delta.y / f32::from(controls.zoom)).round();
        drag.0 = Some(current);
        controls.follow_selected = false;
    }
    if mouse.just_released(MouseButton::Right) {
        drag.0 = None;
    }
}

fn bounded_zoom(current: u8, scroll: f32) -> u8 {
    let step = if scroll.is_sign_positive() { 1 } else { -1 };
    (i16::from(current) + step).clamp(1, 4) as u8
}

/// The authoritative current position, rather than an interpolated render
/// coordinate, determines which storey a following camera presents.
fn followed_floor(authoritative_floor: u8) -> u8 {
    authoritative_floor.min(1)
}

fn follow_selected_resident(
    mut controls: ResMut<CottageCamera>,
    selected: Res<SelectedResident>,
    driver: Res<SimulationDriver>,
    residents: Query<(&ResidentVisual, &GlobalTransform)>,
    mut camera: Query<&mut Transform, With<CottageCameraEntity>>,
) {
    if !controls.follow_selected {
        return;
    }
    let Some(id) = selected.0 else {
        return;
    };
    let Some(snapshot) = driver
        .current
        .residents
        .iter()
        .find(|resident| resident.id == id)
    else {
        return;
    };
    // A selected resident changes the visible storey as soon as their
    // authoritative snapshot reports arrival through the stair portal.
    controls.focused_floor = followed_floor(snapshot.position.floor);
    let Some((_, resident)) = residents.iter().find(|(visual, _)| visual.id == id) else {
        return;
    };
    let Ok(mut camera) = camera.single_mut() else {
        return;
    };
    camera.translation.x = resident.translation().x.round();
    camera.translation.y = resident.translation().y.round();
}

fn update_resident_floor_layers(
    driver: Res<SimulationDriver>,
    mut layers: Query<(&ResidentVisual, &mut FloorVisual)>,
) {
    for (visual, mut floor) in &mut layers {
        if let Some(resident) = driver
            .current
            .residents
            .iter()
            .find(|resident| resident.id == visual.id)
        {
            floor.0 = resident.position.floor;
        }
    }
}

fn apply_floor_focus(
    controls: Res<CottageCamera>,
    mut visuals: Query<(Ref<FloorVisual>, &mut Visibility)>,
) {
    // A resident's authoritative snapshot can move it through a stair portal
    // while camera focus itself remains unchanged (for example, when follow
    // is off). In that case its `FloorVisual` changes and must immediately
    // receive the current focus visibility as well.
    let floor_changed = visuals.iter().any(|(floor, _)| floor.is_changed());
    if !controls.is_changed() && !floor_changed {
        return;
    }
    for (floor, mut visibility) in &mut visuals {
        *visibility = if floor.0 == controls.focused_floor {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

fn select_resident_from_sprite(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    residents: Query<(&ResidentVisual, &GlobalTransform, &Visibility)>,
    mut selected: ResMut<SelectedResident>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, camera_transform)) = camera.single() else {
        return;
    };
    let Ok(world) = camera.viewport_to_world_2d(camera_transform, cursor) else {
        return;
    };
    let selected_id = residents
        .iter()
        .filter_map(|(resident, transform, visibility)| {
            if *visibility != Visibility::Visible {
                return None;
            }
            let distance = transform.translation().truncate().distance(world);
            (distance <= TILE_PIXELS * 0.65).then_some((resident.id, distance))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(id, _)| id);
    if selected_id.is_some() {
        selected.0 = selected_id;
    }
}

/// A short right click on visible ground creates an ordinary typed movement
/// task for the selected newcomer. Right-drag remains camera panning.
#[allow(clippy::too_many_arguments)] // Input, camera, simulation, and UI state stay separate resources.
fn queue_selected_go_to(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera: Query<(&Camera, &GlobalTransform), With<CottageCameraEntity>>,
    drag: Res<PanDrag>,
    controls: Res<CottageCamera>,
    selected: Res<SelectedResident>,
    mut driver: ResMut<SimulationDriver>,
    mut order: ResMut<OrderState>,
) {
    if !mouse.just_released(MouseButton::Right) || order.pending.is_some() {
        return;
    }
    let (Some(resident), Some(start)) = (selected.0, drag.0) else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    if cursor.distance(start) > 4.0 {
        return;
    }
    let Ok((camera, camera_transform)) = camera.single() else {
        return;
    };
    let Ok(world) = camera.viewport_to_world_2d(camera_transform, cursor) else {
        return;
    };
    let local = world - floor_offset(controls.focused_floor);
    let destination = TilePosition {
        floor: controls.focused_floor,
        x: (local.x / TILE_PIXELS).floor() as i32,
        y: (local.y / TILE_PIXELS).floor() as i32,
    };
    let task = PlayerTaskId(order.next_task_id);
    order.next_task_id += 1;
    driver
        .simulation
        .submit_player_command(PlayerCommand::QueueGoTo {
            task,
            resident,
            destination,
            priority: 100,
        });
    order.pending = Some(PendingOrder {
        task,
        action: PendingAction::Order,
        receipt_start: driver.simulation.event_ledger().len(),
    });
    order.receipt = None;
}

#[allow(clippy::type_complexity)] // The ParamSet guarantees the marker write is disjoint.
fn update_selected_resident_marker(
    selected: Res<SelectedResident>,
    controls: Res<CottageCamera>,
    mut visuals: ParamSet<(
        Query<(&ResidentVisual, &GlobalTransform, &FloorVisual), Without<SelectedResidentMarker>>,
        Query<(&mut Transform, &mut Visibility, &mut FloorVisual), With<SelectedResidentMarker>>,
    )>,
) {
    let Some(id) = selected.0 else {
        if let Ok((_, mut visibility, _)) = visuals.p1().single_mut() {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    let Some((_, resident_transform, resident_floor)) = visuals
        .p0()
        .iter()
        .find(|(resident, _, _)| resident.id == id)
        .map(|(resident, transform, floor)| (resident.id, transform.compute_transform(), floor.0))
    else {
        if let Ok((_, mut visibility, _)) = visuals.p1().single_mut() {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    let mut markers = visuals.p1();
    let Ok((mut transform, mut visibility, mut floor)) = markers.single_mut() else {
        return;
    };
    transform.translation = resident_transform.translation;
    transform.translation.z = 3.0;
    // The transform is deliberately interpolated between snapshots, and can
    // therefore still be drawn on the old storey while a resident has already
    // crossed a stair portal. The resident visual floor is updated from the
    // authoritative snapshot before this system runs, so use it rather than
    // inferring a floor from rendered coordinates.
    floor.0 = resident_floor;
    // The marker is a separate visual from the resident layers, so it does
    // not inherit `apply_floor_focus`. Keep it on the same visible storey as
    // its resident; selecting someone on an unfocused floor must not reveal
    // a stray highlight through the house.
    *visibility = if floor.0 == controls.focused_floor {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
}

fn update_clock_text(driver: Res<SimulationDriver>, mut text: Query<&mut Text, With<ClockText>>) {
    if !driver.is_changed() {
        return;
    }
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    text.0 = format_clock(driver.current.time_of_day);
}

fn update_status_card(
    selected: Res<SelectedResident>,
    driver: Res<SimulationDriver>,
    mut text: Query<&mut Text, With<StatusCardText>>,
) {
    if !selected.is_changed() && !driver.is_changed() {
        return;
    }
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    let value = selected_status_text(selected.0, &driver.current);
    text.0 = value;
}

fn selected_status_text(selected: Option<SimId>, snapshot: &CottageSnapshot) -> String {
    let base = selected
        .and_then(|id| snapshot.residents.iter().find(|resident| resident.id == id))
        .map_or_else(
            || "Select a newcomer to inspect them.".to_owned(),
            resident_status_text,
        );
    if snapshot.household_knows_pargeter_custom {
        format!(
            "{base}\n\nThe household learned: the corner seat is Mr Pargeter's (he got the news about the pig there)."
        )
    } else {
        base
    }
}

fn resident_status_text(resident: &village_sim::ClientResidentSnapshot) -> String {
    let need = resident
        .toilet_need
        .map_or_else(|| "not applicable".to_owned(), |value| value.to_string());
    let intention = match resident.autonomous_intention {
        Some(ClientIntention::Toilet) => "Use toilet",
        None => "None",
    };
    let tasks = if resident.player_tasks.is_empty() {
        "Current tasks:\n- None".to_owned()
    } else {
        let mut lines = String::from("Current tasks:");
        for task in &resident.player_tasks {
            let state = match task.state {
                ClientPlayerTaskState::Queued => "queued",
                ClientPlayerTaskState::Active => "active",
                ClientPlayerTaskState::Paused => "paused",
            };
            lines.push_str(&format!("\n- #{} {state}", task.id.0));
        }
        lines
    };
    let perception = match resident.recent_perception {
        Some(ClientPerception::TookPargeterSeat) => "\nPerception: sat in the corner seat",
        Some(ClientPerception::WitnessedPargeterSeat) => "\nPerception: tutted at the corner seat",
        None => "",
    };
    let commitment = if resident.attending_quiz {
        "\nTonight: the pub quiz"
    } else {
        ""
    };
    format!(
        "{}\nToilet need: {need}\nIntention: {intention}\n{tasks}{perception}{commitment}",
        resident.display_name
    )
}

/// Makes a per-resident clothing texture once the source atlas has loaded.
/// Pixels outside the clothing row are transparent in the derived texture,
/// which prevents this asset from ever being used to recolour hair or skin.
fn bake_clothing_hues(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut clothing: Query<(Entity, &ClothingHue, &mut Sprite)>,
) {
    for (entity, hue, mut sprite) in &mut clothing {
        let Some(mut recoloured) = images.get(&sprite.image).cloned() else {
            continue;
        };
        let size = recoloured.texture_descriptor.size;
        let row_height = size.height / 12;
        let clothing_row = (CLOTHING_FIRST_INDEX as u32 / CHARACTER_COLUMNS) * row_height;
        let clothing_end = clothing_row + row_height;
        let Some(data) = recoloured.data.as_mut() else {
            continue;
        };

        // `global.png` is an RGBA8 atlas. Keep only its clothing frame row
        // and rotate the hue of opaque pixels, retaining alpha and HSV value.
        for y in 0..size.height {
            for x in 0..size.width {
                let offset = ((y * size.width + x) * 4) as usize;
                if y < clothing_row || y >= clothing_end {
                    data[offset..offset + 4].fill(0);
                } else if data[offset + 3] != 0 {
                    let rotated = rotate_hue(
                        [data[offset], data[offset + 1], data[offset + 2]],
                        hue.degrees,
                    );
                    data[offset..offset + 3].copy_from_slice(&rotated);
                }
            }
        }
        sprite.image = images.add(recoloured);
        commands.entity(entity).remove::<ClothingHue>();
    }
}

/// Rotates hue in HSV while preserving saturation and value (the shading).
fn rotate_hue([red, green, blue]: [u8; 3], degrees: f32) -> [u8; 3] {
    let red = f32::from(red) / 255.0;
    let green = f32::from(green) / 255.0;
    let blue = f32::from(blue) / 255.0;
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let chroma = maximum - minimum;
    if chroma == 0.0 {
        return [
            (red * 255.0).round() as u8,
            (green * 255.0).round() as u8,
            (blue * 255.0).round() as u8,
        ];
    }
    let hue = if maximum == red {
        ((green - blue) / chroma).rem_euclid(6.0)
    } else if maximum == green {
        (blue - red) / chroma + 2.0
    } else {
        (red - green) / chroma + 4.0
    };
    let hue = (hue * 60.0 + degrees).rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = if hue < 1.0 {
        (chroma, x, 0.0)
    } else if hue < 2.0 {
        (x, chroma, 0.0)
    } else if hue < 3.0 {
        (0.0, chroma, x)
    } else if hue < 4.0 {
        (0.0, x, chroma)
    } else if hue < 5.0 {
        (x, 0.0, chroma)
    } else {
        (chroma, 0.0, x)
    };
    let value_offset = minimum;
    [
        ((r + value_offset) * 255.0).round() as u8,
        ((g + value_offset) * 255.0).round() as u8,
        ((b + value_offset) * 255.0).round() as u8,
    ]
}

fn advance_simulation(time: Res<Time>, mut driver: ResMut<SimulationDriver>) {
    driver.tick_timer.tick(time.delta());
    // `just_finished()` remains true until the next `tick` call. A `while`
    // around it therefore never exits on the first 250 ms simulation tick.
    // Bevy records exactly how many intervals elapsed in this frame instead.
    for _ in 0..driver.tick_timer.times_finished_this_tick() {
        driver.previous = driver.current.clone();
        driver.simulation.advance_tick();
        driver.current = driver.simulation.cottage_snapshot();
    }
}

fn interpolate_residents(
    driver: Res<SimulationDriver>,
    mut residents: Query<(&ResidentVisual, &mut Transform)>,
) {
    let alpha = driver.tick_timer.fraction();
    for (visual, mut transform) in &mut residents {
        let Some(previous) = driver
            .previous
            .residents
            .iter()
            .find(|resident| resident.id == visual.id)
        else {
            continue;
        };
        let Some(current) = driver
            .current
            .residents
            .iter()
            .find(|resident| resident.id == visual.id)
        else {
            continue;
        };
        let from = tile_to_world(previous.position.x, previous.position.y)
            + floor_offset(previous.position.floor);
        let to = tile_to_world(current.position.x, current.position.y)
            + floor_offset(current.position.floor);
        let position = from.lerp(to, alpha);
        transform.translation.x = position.x;
        transform.translation.y = position.y;
    }
}

fn animate_walking(
    time: Res<Time>,
    driver: Res<SimulationDriver>,
    mut layers: Query<(&ResidentVisual, &WalkFrames, &mut Sprite)>,
) {
    let frame = ((time.elapsed_secs() * 8.0) as usize) % 8;
    for (resident, walk, mut sprite) in &mut layers {
        let Some(previous) = driver
            .previous
            .residents
            .iter()
            .find(|candidate| candidate.id == resident.id)
        else {
            continue;
        };
        let Some(current) = driver
            .current
            .residents
            .iter()
            .find(|candidate| candidate.id == resident.id)
        else {
            continue;
        };
        if previous.position != current.position
            && let Some(atlas) = &mut sprite.texture_atlas
        {
            atlas.index = walk.first_index + frame;
        }
    }
}

fn floor_offset(floor: u8) -> Vec2 {
    Vec2::new(0.0, f32::from(floor) * 28.0 * TILE_PIXELS)
}

fn spawn_floor(
    commands: &mut Commands,
    snapshot: &CottageSnapshot,
    floor: u8,
    offset: Vec2,
    house: Handle<Image>,
    layout: Handle<TextureAtlasLayout>,
) {
    let Some(definition) = snapshot
        .floors
        .iter()
        .find(|candidate| candidate.floor == floor)
    else {
        return;
    };
    let size = Vec2::new(
        definition.width as f32 * TILE_PIXELS,
        definition.height as f32 * TILE_PIXELS,
    );
    commands.spawn((
        Sprite::from_color(Color::srgb(0.18, 0.22, 0.26), size),
        Transform::from_xyz(offset.x + size.x / 2.0, offset.y + size.y / 2.0, -1.0),
        FloorVisual(floor),
    ));
    for y in 0..definition.height {
        for x in 0..definition.width {
            let position = tile_to_world(x, y) + offset;
            commands.spawn((
                Sprite::from_atlas_image(
                    house.clone(),
                    TextureAtlas {
                        layout: layout.clone(),
                        index: 0,
                    },
                ),
                Transform::from_xyz(position.x, position.y, 0.0),
                FloorVisual(floor),
            ));
        }
    }
}

fn tile_to_world(x: i32, y: i32) -> Vec2 {
    Vec2::new(
        (x as f32 + 0.5) * TILE_PIXELS,
        (y as f32 + 0.5) * TILE_PIXELS,
    )
}

/// The authoritative scenario content for the Cottage. On the web there is no
/// filesystem, so the content is compiled in; native builds read the authored
/// files so they can be edited without recompiling the simulation.
#[cfg(target_arch = "wasm32")]
fn cottage_content() -> ScenarioContent {
    ScenarioContent::embedded_cottage_arrival().expect("embedded Cottage content resolves")
}

#[cfg(not(target_arch = "wasm32"))]
fn cottage_content() -> ScenarioContent {
    ScenarioContent::load_cottage_arrival(content_root()).expect("Cottage content loads")
}

/// The Bevy asset root. On the web this is a relative path served alongside the
/// page; native builds point at the workspace `assets` directory.
#[cfg(target_arch = "wasm32")]
fn asset_root() -> String {
    "assets".to_owned()
}

#[cfg(not(target_arch = "wasm32"))]
fn asset_root() -> String {
    workspace_root()
        .join("assets")
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(target_arch = "wasm32"))]
fn content_root() -> std::path::PathBuf {
    workspace_root().join("assets/content")
}

#[cfg(not(target_arch = "wasm32"))]
fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use village_sim::{
        ClientIntention, ClientPerception, ClientPlayerTaskSnapshot, ClientPlayerTaskState,
        ClientResidentSnapshot, DefinitionId, PlayerCommand, PlayerCommandRejection, PlayerTaskId,
        ScenarioContent, SimId, Simulation, TilePosition, TimeOfDay, WorldEvent, WorldEventKind,
    };

    use super::{
        CancelBinding, CancellationOutcome, CottageCamera, FloorVisual, OrderState, PendingAction,
        PendingOrder, ResidentVisual, SelectedResident, SelectedResidentMarker, SemanticEventFeed,
        UiAudioVariation, apply_floor_focus, bounded_zoom, cancellable_tasks,
        consume_order_receipt, consume_semantic_events, content_root, deferred_receipt,
        floor_offset, followed_floor, format_clock, furniture_atlas_index, next_ui_pitch,
        resident_status_text, rotate_hue, selected_status_text, tile_to_world,
        update_selected_resident_marker,
    };
    use bevy::prelude::{App, GlobalTransform, Transform, Update, Visibility};

    #[test]
    fn furniture_index_distinguishes_the_bar_from_the_toilet() {
        assert_eq!(
            furniture_atlas_index(&DefinitionId::new("object_type.bar")),
            108
        );
        assert_eq!(
            furniture_atlas_index(&DefinitionId::new("object_type.toilet")),
            144
        );
    }

    #[test]
    fn clock_formats_as_zero_padded_hours_and_minutes() {
        assert_eq!(
            format_clock(TimeOfDay {
                hour: 16,
                minute: 5
            }),
            "16:05"
        );
        assert_eq!(format_clock(TimeOfDay { hour: 9, minute: 0 }), "09:00");
    }

    #[test]
    fn semantic_feed_narrates_the_pargeter_seat_moment() {
        let snapshot = village_sim::CottageSnapshot {
            tick: 60,
            time_of_day: TimeOfDay {
                hour: 17,
                minute: 0,
            },
            household_knows_pargeter_custom: true,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: vec![
                ClientResidentSnapshot {
                    id: SimId(1),
                    definition_id: DefinitionId::new("person.newcomer_a"),
                    display_name: "Rowan Bell".to_owned(),
                    position: TilePosition {
                        floor: 0,
                        x: 22,
                        y: 20,
                    },
                    toilet_need: Some(0),
                    autonomous_intention: None,
                    player_tasks: Vec::new(),
                    recent_perception: Some(ClientPerception::TookPargeterSeat),
                    attending_quiz: false,
                },
                ClientResidentSnapshot {
                    id: SimId(2),
                    definition_id: DefinitionId::new("person.newcomer_b"),
                    display_name: "Mara Bell".to_owned(),
                    position: TilePosition {
                        floor: 0,
                        x: 25,
                        y: 18,
                    },
                    toilet_need: None,
                    autonomous_intention: None,
                    player_tasks: Vec::new(),
                    recent_perception: Some(ClientPerception::WitnessedPargeterSeat),
                    attending_quiz: false,
                },
            ],
        };
        let events = [WorldEvent {
            tick: 60,
            kind: WorldEventKind::PargeterSeatTaken {
                participant: SimId(1),
                witnessed_by: vec![SimId(2)],
            },
        }];
        let mut feed = SemanticEventFeed::default();
        let mut outcomes = BTreeMap::new();

        consume_semantic_events(&mut feed, &events, &snapshot, &mut outcomes);

        assert_eq!(
            feed.entries,
            [
                "Rowan Bell took Mr Pargeter's corner seat. Mara Bell tuts. The household now know it is his."
            ]
        );
    }

    #[test]
    fn semantic_feed_announces_quiz_arrival() {
        let snapshot = village_sim::CottageSnapshot {
            tick: 200,
            time_of_day: TimeOfDay {
                hour: 19,
                minute: 20,
            },
            household_knows_pargeter_custom: false,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: vec![ClientResidentSnapshot {
                id: SimId(2),
                definition_id: DefinitionId::new("person.newcomer_b"),
                display_name: "Mara Bell".to_owned(),
                position: TilePosition {
                    floor: 0,
                    x: 25,
                    y: 18,
                },
                toilet_need: None,
                autonomous_intention: None,
                player_tasks: Vec::new(),
                recent_perception: None,
                attending_quiz: false,
            }],
        };
        let events = [WorldEvent {
            tick: 200,
            kind: WorldEventKind::QuizArrived { resident: SimId(2) },
        }];
        let mut feed = SemanticEventFeed::default();
        let mut outcomes = BTreeMap::new();

        consume_semantic_events(&mut feed, &events, &snapshot, &mut outcomes);

        assert_eq!(
            feed.entries,
            ["Mara Bell arrived at the King's Head for the quiz."]
        );
    }

    #[test]
    fn semantic_feed_announces_a_neighbour_invitation() {
        let snapshot = village_sim::CottageSnapshot {
            tick: 30,
            time_of_day: TimeOfDay {
                hour: 16,
                minute: 30,
            },
            household_knows_pargeter_custom: false,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: Vec::new(),
        };
        let events = [WorldEvent {
            tick: 30,
            kind: WorldEventKind::NeighbourInvitation {
                host: DefinitionId::new("person.neighbour"),
                event_at: TimeOfDay {
                    hour: 19,
                    minute: 0,
                },
            },
        }];
        let mut feed = SemanticEventFeed::default();
        let mut outcomes = BTreeMap::new();

        consume_semantic_events(&mut feed, &events, &snapshot, &mut outcomes);

        assert_eq!(
            feed.entries,
            ["A neighbour invited the household to the 19:00 quiz."]
        );
    }

    #[test]
    fn tiles_project_to_their_pixel_centres() {
        assert_eq!(tile_to_world(0, 0).as_ivec2().to_array(), [16, 16]);
        assert_eq!(tile_to_world(3, 2).as_ivec2().to_array(), [112, 80]);
    }

    #[test]
    fn hue_rotation_preserves_greyscale_shading() {
        assert_eq!(rotate_hue([37, 37, 37], 202.0), [37, 37, 37]);
    }

    #[test]
    fn hue_rotation_changes_chromatic_clothing_pixels() {
        assert_eq!(rotate_hue([255, 0, 0], 120.0), [0, 255, 0]);
    }

    #[test]
    fn resident_card_uses_only_projected_authoritative_status() {
        let resident = ClientResidentSnapshot {
            id: SimId(1),
            definition_id: DefinitionId::new("person.newcomer_a"),
            display_name: "Rowan Bell".to_owned(),
            position: TilePosition {
                floor: 0,
                x: 2,
                y: 3,
            },
            toilet_need: Some(50),
            autonomous_intention: Some(ClientIntention::Toilet),
            player_tasks: vec![
                ClientPlayerTaskSnapshot {
                    id: PlayerTaskId(7),
                    state: ClientPlayerTaskState::Active,
                },
                ClientPlayerTaskSnapshot {
                    id: PlayerTaskId(8),
                    state: ClientPlayerTaskState::Queued,
                },
                ClientPlayerTaskSnapshot {
                    id: PlayerTaskId(9),
                    state: ClientPlayerTaskState::Paused,
                },
            ],
            recent_perception: None,
            attending_quiz: false,
        };

        assert_eq!(
            resident_status_text(&resident),
            "Rowan Bell\nToilet need: 50\nIntention: Use toilet\nCurrent tasks:\n- #7 active\n- #8 queued\n- #9 paused"
        );
    }

    #[test]
    fn resident_card_has_a_clear_no_selection_state() {
        let snapshot = village_sim::CottageSnapshot {
            tick: 0,
            time_of_day: village_sim::TimeOfDay { hour: 0, minute: 0 },
            household_knows_pargeter_custom: false,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: Vec::new(),
        };

        assert_eq!(
            selected_status_text(None, &snapshot),
            "Select a newcomer to inspect them."
        );
    }

    #[test]
    fn camera_zoom_is_bounded_to_integral_pixel_scales() {
        assert_eq!(bounded_zoom(1, -1.0), 1);
        assert_eq!(bounded_zoom(4, 1.0), 4);
        assert_eq!(bounded_zoom(2, 1.0), 3);
    }

    #[test]
    fn follow_uses_authoritative_destination_floor() {
        assert_eq!(followed_floor(0), 0);
        assert_eq!(followed_floor(1), 1);
    }

    #[test]
    fn focused_floor_visibility_updates_when_authoritative_floor_changes() {
        let mut app = App::new();
        app.insert_resource(CottageCamera {
            follow_selected: false,
            focused_floor: 1,
            zoom: 2,
        });
        app.add_systems(Update, apply_floor_focus);
        let resident = app
            .world_mut()
            .spawn((FloorVisual(0), Visibility::Visible))
            .id();

        // Establish the initial state, then change only the visual floor as
        // `update_resident_floor_layers` does after a simulation snapshot.
        app.update();
        app.world_mut().clear_trackers();
        app.world_mut()
            .entity_mut(resident)
            .get_mut::<FloorVisual>()
            .unwrap()
            .0 = 1;
        app.update();

        assert_eq!(
            *app.world().entity(resident).get::<Visibility>().unwrap(),
            Visibility::Visible
        );
    }

    #[test]
    fn selected_marker_stays_hidden_when_its_floor_is_not_focused() {
        let mut app = App::new();
        app.insert_resource(CottageCamera {
            follow_selected: false,
            focused_floor: 0,
            zoom: 2,
        });
        app.insert_resource(SelectedResident(Some(SimId(1))));
        app.add_systems(Update, update_selected_resident_marker);
        app.world_mut().spawn((
            ResidentVisual { id: SimId(1) },
            GlobalTransform::from(Transform::from_xyz(16.0, floor_offset(1).y + 16.0, 1.0)),
            FloorVisual(1),
        ));
        let marker = app
            .world_mut()
            .spawn((
                Transform::default(),
                Visibility::Hidden,
                SelectedResidentMarker,
                FloorVisual(0),
            ))
            .id();

        app.update();

        assert_eq!(
            *app.world().entity(marker).get::<Visibility>().unwrap(),
            Visibility::Hidden
        );
    }

    #[test]
    fn selected_marker_uses_authoritative_floor_during_stair_transition() {
        let mut app = App::new();
        // This is the focus a following camera adopts as soon as the current
        // snapshot reports the resident through the stair portal.
        app.insert_resource(CottageCamera {
            follow_selected: true,
            focused_floor: 1,
            zoom: 2,
        });
        app.insert_resource(SelectedResident(Some(SimId(1))));
        app.add_systems(Update, update_selected_resident_marker);
        // Interpolation is still visibly on the ground floor, but the
        // authoritative floor layer has already moved to the upper storey.
        app.world_mut().spawn((
            ResidentVisual { id: SimId(1) },
            GlobalTransform::from(Transform::from_xyz(16.0, 16.0, 1.0)),
            FloorVisual(1),
        ));
        let marker = app
            .world_mut()
            .spawn((
                Transform::default(),
                Visibility::Hidden,
                SelectedResidentMarker,
                FloorVisual(0),
            ))
            .id();

        app.update();

        assert_eq!(
            app.world().entity(marker).get::<FloorVisual>().unwrap().0,
            1
        );
        assert_eq!(
            *app.world().entity(marker).get::<Visibility>().unwrap(),
            Visibility::Visible
        );

        // Manually returning focus to the ground floor must hide the same
        // upper-storey marker, rather than leaving a stale visual behind.
        app.world_mut()
            .resource_mut::<CottageCamera>()
            .focused_floor = 0;
        app.update();

        assert_eq!(
            app.world().entity(marker).get::<FloorVisual>().unwrap().0,
            1
        );
        assert_eq!(
            *app.world().entity(marker).get::<Visibility>().unwrap(),
            Visibility::Hidden
        );
    }

    #[test]
    fn pending_order_consumes_only_its_own_deferred_receipt() {
        let task = PlayerTaskId(12);
        let events = [
            WorldEvent {
                tick: 4,
                kind: WorldEventKind::PlayerCommandAccepted {
                    task: PlayerTaskId(11),
                },
            },
            WorldEvent {
                tick: 4,
                kind: WorldEventKind::PlayerCommandAccepted { task },
            },
        ];

        assert_eq!(
            deferred_receipt(
                PendingOrder {
                    task,
                    action: PendingAction::Order,
                    receipt_start: 0,
                },
                &events
            ),
            Some("Order #12 accepted.".to_owned())
        );
    }

    #[test]
    fn cancellation_receipt_is_named_as_a_cancellation() {
        let task = PlayerTaskId(27);
        let events = [WorldEvent {
            tick: 8,
            kind: WorldEventKind::PlayerCommandAccepted { task },
        }];

        assert_eq!(
            deferred_receipt(
                PendingOrder {
                    task,
                    action: PendingAction::Cancellation(CancellationOutcome::Queued),
                    receipt_start: 0,
                },
                &events,
            ),
            Some("Cancellation #27 accepted.".to_owned())
        );
    }

    #[test]
    fn cancellation_receipt_ignores_the_orders_earlier_receipt() {
        let task = PlayerTaskId(271);
        let events = [
            WorldEvent {
                tick: 8,
                kind: WorldEventKind::PlayerCommandAccepted { task },
            },
            WorldEvent {
                tick: 9,
                kind: WorldEventKind::PlayerCommandAccepted { task },
            },
        ];

        assert_eq!(
            deferred_receipt(
                PendingOrder {
                    task,
                    action: PendingAction::Cancellation(CancellationOutcome::Active),
                    receipt_start: 1,
                },
                &events,
            ),
            Some("Cancellation #271 accepted.".to_owned())
        );
    }

    #[test]
    fn semantic_event_feed_consumes_wait_and_active_cancellation_once() {
        let task = PlayerTaskId(28);
        let snapshot = village_sim::CottageSnapshot {
            tick: 8,
            time_of_day: village_sim::TimeOfDay {
                hour: 20,
                minute: 15,
            },
            household_knows_pargeter_custom: false,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: Vec::new(),
        };
        let events = [
            WorldEvent {
                tick: 8,
                kind: WorldEventKind::ObjectUseWaited {
                    resident: SimId(1),
                    object: DefinitionId::new("object.cottage_toilet"),
                    affordance: DefinitionId::new("affordance.use_toilet"),
                    blocked_by: SimId(2),
                },
            },
            WorldEvent {
                tick: 8,
                kind: WorldEventKind::TaskCancelled {
                    task,
                    resident: SimId(2),
                    object: DefinitionId::new("object.cottage_toilet"),
                    affordance: DefinitionId::new("affordance.use_toilet"),
                },
            },
        ];
        let mut feed = SemanticEventFeed::default();
        let mut outcomes = BTreeMap::from([(task, CancellationOutcome::Active)]);

        consume_semantic_events(&mut feed, &events, &snapshot, &mut outcomes);
        consume_semantic_events(&mut feed, &events, &snapshot, &mut outcomes);

        assert_eq!(feed.entries.len(), 2);
        assert_eq!(feed.entries[0], "A newcomer is waiting for the toilet.");
        assert_eq!(
            feed.entries[1],
            "Order #28 for A newcomer cancelled; toilet released."
        );
        assert!(outcomes.is_empty());
    }

    #[test]
    fn semantic_event_feed_labels_a_queued_cancellation_as_removed_from_queue() {
        let task = PlayerTaskId(30);
        let snapshot = village_sim::CottageSnapshot {
            tick: 8,
            time_of_day: village_sim::TimeOfDay {
                hour: 20,
                minute: 15,
            },
            household_knows_pargeter_custom: false,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: Vec::new(),
        };
        let events = [WorldEvent {
            tick: 8,
            kind: WorldEventKind::TaskCancelled {
                task,
                resident: SimId(2),
                object: DefinitionId::new("object.cottage_toilet"),
                affordance: DefinitionId::new("affordance.use_toilet"),
            },
        }];
        let mut feed = SemanticEventFeed::default();
        let mut outcomes = BTreeMap::from([(task, CancellationOutcome::Queued)]);

        consume_semantic_events(&mut feed, &events, &snapshot, &mut outcomes);

        assert_eq!(
            feed.entries,
            ["Order #30 for A newcomer cancelled; removed from queue."],
        );
        assert!(outcomes.is_empty());
    }

    #[test]
    fn cancellable_tasks_project_the_whole_queue_head_first() {
        let snapshot = village_sim::CottageSnapshot {
            tick: 0,
            time_of_day: village_sim::TimeOfDay { hour: 0, minute: 0 },
            household_knows_pargeter_custom: false,
            floors: Vec::new(),
            objects: Vec::new(),
            residents: vec![ClientResidentSnapshot {
                id: SimId(3),
                definition_id: DefinitionId::new("person.newcomer_a"),
                display_name: "Rowan Bell".to_owned(),
                position: TilePosition {
                    floor: 0,
                    x: 0,
                    y: 0,
                },
                toilet_need: Some(50),
                autonomous_intention: None,
                player_tasks: vec![
                    ClientPlayerTaskSnapshot {
                        id: PlayerTaskId(29),
                        state: ClientPlayerTaskState::Active,
                    },
                    ClientPlayerTaskSnapshot {
                        id: PlayerTaskId(31),
                        state: ClientPlayerTaskState::Paused,
                    },
                ],
                recent_perception: None,
                attending_quiz: false,
            }],
        };

        assert_eq!(
            cancellable_tasks(Some(SimId(3)), &snapshot),
            vec![
                CancelBinding {
                    task: PlayerTaskId(29),
                    outcome: CancellationOutcome::Active,
                },
                CancelBinding {
                    task: PlayerTaskId(31),
                    outcome: CancellationOutcome::Paused,
                },
            ]
        );
        assert!(cancellable_tasks(Some(SimId(4)), &snapshot).is_empty());
        assert!(cancellable_tasks(None, &snapshot).is_empty());
    }

    #[test]
    fn deferred_rejection_has_a_player_readable_reason() {
        let task = PlayerTaskId(19);
        let events = [WorldEvent {
            tick: 5,
            kind: WorldEventKind::PlayerCommandRejected {
                task,
                reason: PlayerCommandRejection::ResidentBusy,
            },
        }];

        assert_eq!(
            deferred_receipt(
                PendingOrder {
                    task,
                    action: PendingAction::Order,
                    receipt_start: 0,
                },
                &events
            ),
            Some("Order #19 rejected: resident busy.".to_owned())
        );
    }

    #[test]
    fn order_feedback_uses_the_receipt_published_on_its_next_client_advance() {
        let content = ScenarioContent::load_cottage_arrival(content_root()).expect("fixture loads");
        let mut simulation = Simulation::from_content(content).expect("fixture resolves");
        let resident = simulation.cottage_snapshot().residents[0].id;
        let task = PlayerTaskId(44);
        let mut order = OrderState {
            next_task_id: 45,
            pending: Some(PendingOrder {
                task,
                action: PendingAction::Order,
                receipt_start: 0,
            }),
            receipt: None,
            ..OrderState::default()
        };

        simulation.submit_player_command(PlayerCommand::QueueUseToilet {
            task,
            resident,
            object: DefinitionId::new("object.cottage_toilet"),
            affordance: DefinitionId::new("affordance.use_toilet"),
            priority: 100,
        });

        // The receipt is not published until the client performs its next
        // simulation advance.
        consume_order_receipt(&mut order, simulation.event_ledger());
        assert!(order.receipt.is_none());
        assert!(order.pending.is_some());

        simulation.advance_tick();
        consume_order_receipt(&mut order, simulation.event_ledger());

        assert_eq!(order.receipt, Some("Order #44 accepted.".to_owned()));
        assert!(order.pending.is_none());
    }

    #[test]
    fn ui_pitch_variation_is_bounded_and_client_local() {
        let mut variation = UiAudioVariation(7);
        for _ in 0..32 {
            assert!((0.94..=1.06).contains(&next_ui_pitch(&mut variation)));
        }
    }
}
