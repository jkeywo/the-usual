//! Read-only Bevy presentation for the Cottage Contention fixture.

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::{asset::AssetPlugin, input::mouse::MouseWheel};
use village_sim::{
    ClientIntention, ClientPlayerTaskState, CottageSnapshot, DefinitionId, PlayerCommand,
    PlayerCommandRejection, PlayerTaskId, ScenarioContent, SimId, Simulation, WorldEvent,
    WorldEventKind,
};

const TILE_PIXELS: f32 = 32.0;
const CHARACTER_COLUMNS: u32 = 8;
const CLOTHING_FIRST_INDEX: usize = 32;

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
            zoom: 2,
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

#[derive(Component)]
struct OrderFeedbackText;

#[derive(Component)]
struct UseToiletButton;

/// Allocates task IDs in the client only. An allocated ID is retained by the
/// pending order until the simulation publishes its immutable receipt.
#[derive(Default, Resource)]
struct OrderState {
    next_task_id: u64,
    pending: Option<PendingOrder>,
    receipt: Option<String>,
}

#[derive(Clone, Copy)]
struct PendingOrder {
    task: PlayerTaskId,
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
                    file_path: workspace_root()
                        .join("assets")
                        .to_string_lossy()
                        .into_owned(),
                    ..default()
                }),
        )
        .add_systems(Startup, setup_cottage)
        .add_systems(
            Update,
            (
                bake_clothing_hues,
                advance_simulation,
                interpolate_residents,
                animate_walking,
                select_resident_from_sprite,
                select_resident_from_card,
                submit_selected_toilet_order,
                update_camera_controls,
                update_camera_control_labels,
                pan_and_zoom_camera,
                follow_selected_resident,
                update_resident_floor_layers,
                update_selected_resident_marker,
                apply_floor_focus,
                update_status_card,
                update_order_feedback,
                update_order_feedback_text,
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
    let simulation = Simulation::from_content(
        ScenarioContent::load_cottage_arrival(content_root()).expect("Cottage content loads"),
    )
    .expect("Cottage content resolves");
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
    commands.insert_resource(UiAudioVariation(client_audio_seed()));
    commands.spawn((Camera2d, CottageCameraEntity));
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
            Transform::from_xyz(position.x, position.y, 4.0),
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
            Transform::from_xyz(position.x, position.y, 5.0),
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
                    index: 48,
                },
            ),
            Transform::from_xyz(position.x, position.y, 2.0),
            Name::new(object.id.0.clone()),
            FloorVisual(object.position.floor),
        ));
    }

    spawn_status_card(&mut commands, &asset_server, &snapshot);
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
    if order.pending.is_some() || selected_resident_has_player_task(resident, &driver.current) {
        order.receipt = Some("That newcomer already has a player task.".to_owned());
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
    order.pending = Some(PendingOrder { task });
    order.receipt = None;
    play_ui_click(&mut commands, &asset_server, &mut audio_variation);
}

fn selected_resident_has_player_task(resident: SimId, snapshot: &CottageSnapshot) -> bool {
    snapshot
        .residents
        .iter()
        .find(|candidate| candidate.id == resident)
        .is_some_and(|candidate| candidate.player_task.is_some())
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
    let Some(receipt) = deferred_receipt(pending.task, events) else {
        return;
    };
    order.pending = None;
    order.receipt = Some(receipt);
}

fn deferred_receipt(task: PlayerTaskId, events: &[WorldEvent]) -> Option<String> {
    events.iter().find_map(|event| match &event.kind {
        WorldEventKind::PlayerCommandAccepted { task: received } if *received == task => {
            Some(format!("Order #{task_id} accepted.", task_id = task.0))
        }
        WorldEventKind::PlayerCommandRejected {
            task: received,
            reason,
        } if *received == task => Some(format!(
            "Order #{task_id} rejected: {reason}.",
            task_id = task.0,
            reason = rejection_label(*reason)
        )),
        _ => None,
    })
}

fn rejection_label(reason: PlayerCommandRejection) -> &'static str {
    match reason {
        PlayerCommandRejection::DuplicateTask => "duplicate task",
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

fn client_audio_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64)
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
            (transform.translation.y - delta.y / f32::from(controls.zoom)).round();
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
    selected
        .and_then(|id| snapshot.residents.iter().find(|resident| resident.id == id))
        .map_or_else(
            || "Select a newcomer to inspect them.".to_owned(),
            resident_status_text,
        )
}

fn resident_status_text(resident: &village_sim::ClientResidentSnapshot) -> String {
    let need = resident
        .toilet_need
        .map_or_else(|| "not applicable".to_owned(), |value| value.to_string());
    let intention = match resident.autonomous_intention {
        Some(ClientIntention::Toilet) => "Use toilet",
        None => "None",
    };
    let task = resident.player_task.map_or_else(
        || "None".to_owned(),
        |task| match task.state {
            ClientPlayerTaskState::Queued => format!("#{} queued", task.id.0),
            ClientPlayerTaskState::Active => format!("#{} active", task.id.0),
        },
    );
    format!(
        "{}\nToilet need: {need}\nIntention: {intention}\nPlayer task: {task}",
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
    while driver.tick_timer.just_finished() {
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

fn content_root() -> PathBuf {
    workspace_root().join("assets/content")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[cfg(test)]
mod tests {
    use village_sim::{
        ClientIntention, ClientPlayerTaskSnapshot, ClientPlayerTaskState, ClientResidentSnapshot,
        DefinitionId, PlayerCommand, PlayerCommandRejection, PlayerTaskId, ScenarioContent, SimId,
        Simulation, TilePosition, WorldEvent, WorldEventKind,
    };

    use super::{
        CottageCamera, FloorVisual, OrderState, PendingOrder, ResidentVisual, SelectedResident,
        SelectedResidentMarker, UiAudioVariation, apply_floor_focus, bounded_zoom,
        consume_order_receipt, content_root, deferred_receipt, floor_offset, followed_floor,
        next_ui_pitch, resident_status_text, rotate_hue, selected_status_text, tile_to_world,
        update_selected_resident_marker,
    };
    use bevy::prelude::{App, GlobalTransform, Transform, Update, Visibility};

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
            player_task: Some(ClientPlayerTaskSnapshot {
                id: PlayerTaskId(7),
                state: ClientPlayerTaskState::Queued,
            }),
        };

        assert_eq!(
            resident_status_text(&resident),
            "Rowan Bell\nToilet need: 50\nIntention: Use toilet\nPlayer task: #7 queued"
        );
    }

    #[test]
    fn resident_card_has_a_clear_no_selection_state() {
        let snapshot = village_sim::CottageSnapshot {
            tick: 0,
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
            deferred_receipt(task, &events),
            Some("Order #12 accepted.".to_owned())
        );
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
            deferred_receipt(task, &events),
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
            pending: Some(PendingOrder { task }),
            receipt: None,
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
