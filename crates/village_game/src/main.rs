//! Read-only Bevy presentation for the Cottage Contention fixture.

use std::path::PathBuf;

use bevy::prelude::*;
use village_sim::{
    ClientIntention, ClientPlayerTaskState, CottageSnapshot, ScenarioContent, SimId, Simulation,
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

#[derive(Component)]
struct ResidentSelector(SimId);

#[derive(Component)]
struct StatusCardText;

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
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
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
                update_selected_resident_marker,
                update_status_card,
            ),
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
    commands.spawn(Camera2d);
    commands.spawn((
        Sprite::from_color(
            Color::srgba(0.95, 0.8, 0.2, 0.45),
            Vec2::splat(TILE_PIXELS + 4.0),
        ),
        Transform::from_xyz(0.0, 0.0, 3.0),
        Visibility::Hidden,
        SelectedResidentMarker,
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
        let position = tile_to_world(object.position.x, object.position.y);
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
        });
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

fn select_resident_from_sprite(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    residents: Query<(&ResidentVisual, &GlobalTransform)>,
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
        .filter_map(|(resident, transform)| {
            let distance = transform.translation().truncate().distance(world);
            (distance <= TILE_PIXELS * 0.65).then_some((resident.id, distance))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(id, _)| id);
    if selected_id.is_some() {
        selected.0 = selected_id;
    }
}

fn update_selected_resident_marker(
    selected: Res<SelectedResident>,
    residents: Query<(&ResidentVisual, &GlobalTransform)>,
    mut marker: Query<(&mut Transform, &mut Visibility), With<SelectedResidentMarker>>,
) {
    let Ok((mut transform, mut visibility)) = marker.single_mut() else {
        return;
    };
    let Some(id) = selected.0 else {
        *visibility = Visibility::Hidden;
        return;
    };
    let Some((_, resident_transform)) = residents.iter().find(|(resident, _)| resident.id == id)
    else {
        *visibility = Visibility::Hidden;
        return;
    };
    transform.translation = resident_transform.translation();
    transform.translation.z = 3.0;
    *visibility = Visibility::Visible;
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
    source_images: Res<Assets<Image>>,
    mut generated_images: ResMut<Assets<Image>>,
    mut clothing: Query<(Entity, &ClothingHue, &mut Sprite)>,
) {
    for (entity, hue, mut sprite) in &mut clothing {
        let Some(source) = source_images.get(&sprite.image) else {
            continue;
        };
        let mut recoloured = source.clone();
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
        sprite.image = generated_images.add(recoloured);
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/content")
}

#[cfg(test)]
mod tests {
    use village_sim::{
        ClientIntention, ClientPlayerTaskSnapshot, ClientPlayerTaskState, ClientResidentSnapshot,
        DefinitionId, PlayerTaskId, SimId, TilePosition,
    };

    use super::{resident_status_text, rotate_hue, selected_status_text, tile_to_world};

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
}
