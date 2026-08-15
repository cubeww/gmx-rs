use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use rayon::prelude::*;

use crate::assets::{
    Action, Assets, Background, CollisionType, Font, GameObject, GamePath, ObjectEvent, Room,
    RoomBackground, RoomInstance, RoomTile, RoomView, Sprite, SpriteType, Timeline, TimelineEntry,
};
use crate::audio::AudioData;
use crate::gml::{CodeKind, CompiledProject};
use crate::shader::{ShaderData, prepare_shaders};
use crate::texture::TextureData;
use crate::wad::{ChunkWriter, FourCc, StringTable, WadBuilder, WriteError};

const SOND: FourCc = FourCc::new(*b"SOND");
const AGRP: FourCc = FourCc::new(*b"AGRP");
const SPRT: FourCc = FourCc::new(*b"SPRT");
const BGND: FourCc = FourCc::new(*b"BGND");
const PATH: FourCc = FourCc::new(*b"PATH");
const SCPT: FourCc = FourCc::new(*b"SCPT");
const GLOB: FourCc = FourCc::new(*b"GLOB");
const SHDR: FourCc = FourCc::new(*b"SHDR");
const FONT: FourCc = FourCc::new(*b"FONT");
const TMLN: FourCc = FourCc::new(*b"TMLN");
const OBJT: FourCc = FourCc::new(*b"OBJT");
const ROOM: FourCc = FourCc::new(*b"ROOM");
const DAFL: FourCc = FourCc::new(*b"DAFL");

#[derive(Debug)]
struct SoundRecord {
    name: String,
    kind: i32,
    extension: String,
    filename: String,
    effects: i32,
    volume: f32,
    pan: f32,
    group_or_preload: i32,
    audio_id: i32,
}

#[derive(Debug)]
struct ScriptRecord {
    name: String,
    code_index: i32,
}

#[derive(Debug)]
struct SpriteRecord<'a> {
    sprite: &'a Sprite,
    masks: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct AlphaFrame {
    width: i32,
    height: i32,
    alpha: Vec<u8>,
}

#[derive(Debug, Default)]
struct CodeIndices {
    object_events: HashMap<String, i32>,
    timeline_entries: HashMap<String, i32>,
    rooms: HashMap<String, i32>,
    room_instances: HashMap<String, i32>,
}

/// Adds the resource chunks that do not depend on texture-page placement.
/// Chunks are registered in their official GMS 1.4 order; later stage-four
/// writers are inserted between these registrations as they are implemented.
pub fn add_resource_chunks<'a>(
    builder: &mut WadBuilder<'a>,
    strings: &'a StringTable,
    assets: &'a Assets,
    compiled: &'a CompiledProject,
    textures: &'a TextureData,
    audio: &'a AudioData,
) -> Result<(), WriteError> {
    let sound_records = sound_records(assets, audio)?;
    builder.add_chunk(SOND, move |writer| {
        writer.write_offset_table(&sound_records, |writer, sound| {
            write_sound(writer, strings, sound)
        })
    })?;

    let audio_groups = assets
        .settings
        .audio_groups
        .iter()
        .filter(|group| !group.implicit)
        .map(|group| group.name.as_str())
        .collect::<Vec<_>>();
    builder.add_chunk(AGRP, move |writer| {
        writer.write_offset_table(&audio_groups, |writer, name| {
            strings.write_reference(writer, name)
        })
    })?;

    let sprites = prepare_sprites(assets)?;
    builder.add_chunk(SPRT, move |writer| {
        write_sprites(writer, strings, &sprites, textures)
    })?;

    builder.add_chunk(BGND, move |writer| {
        writer.write_offset_table(&assets.backgrounds, |writer, background| {
            write_background(writer, strings, background, textures)
        })
    })?;

    builder.add_chunk(PATH, move |writer| {
        writer.write_offset_table(&assets.paths, |writer, path| {
            write_path(writer, strings, path)
        })
    })?;

    let scripts = script_records(compiled)?;
    builder.add_chunk(SCPT, move |writer| {
        writer.write_offset_table(&scripts, |writer, script| {
            strings.write_reference(writer, &script.name)?;
            writer.write_i32(script.code_index)
        })
    })?;

    let globals = compiled
        .codes
        .iter()
        .enumerate()
        .filter(|(_, code)| code.kind == CodeKind::Global)
        .map(|(index, _)| {
            i32::try_from(index).map_err(|_| WriteError::SizeOverflow {
                field: "global CODE index",
                size: index as u64,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    builder.add_chunk(GLOB, move |writer| {
        writer.write_u32(as_u32(globals.len(), "global code count")?)?;
        for index in &globals {
            writer.write_i32(*index)?;
        }
        Ok(())
    })?;

    let shaders = prepare_shaders(&assets.shaders)
        .map_err(|error| invalid_resource(format!("shader preparation failed: {error}")))?;
    builder.add_chunk(SHDR, move |writer| {
        writer.write_offset_table(&shaders, |writer, shader| {
            write_shader(writer, strings, shader)
        })
    })?;

    builder.add_chunk(FONT, move |writer| {
        write_fonts(writer, strings, &assets.fonts, textures)
    })?;

    let code_indices = Rc::new(CodeIndices::new(compiled)?);
    let timeline_code = Rc::clone(&code_indices);
    builder.add_chunk(TMLN, move |writer| {
        writer.write_offset_table(&assets.timelines, |writer, timeline| {
            write_timeline(writer, strings, timeline, &timeline_code)
        })
    })?;

    let object_code = Rc::clone(&code_indices);
    builder.add_chunk(OBJT, move |writer| {
        writer.write_offset_table(&assets.objects, |writer, object| {
            write_object(writer, strings, object, &object_code)
        })
    })?;

    builder.add_chunk(ROOM, move |writer| {
        writer.write_offset_table(&assets.rooms, |writer, room| {
            write_room(writer, strings, room, &code_indices)
        })
    })?;

    // DAFL is a marker chunk. Included files are copied beside data.win and
    // do not have payload records in GMS 1.4.
    builder.add_chunk(DAFL, |_| Ok(()))?;
    Ok(())
}

fn sound_records(assets: &Assets, audio: &AudioData) -> Result<Vec<SoundRecord>, WriteError> {
    let mut records = Vec::with_capacity(assets.sounds.len());
    for sound in &assets.sounds {
        let media = audio.sound(sound.index);
        records.push(SoundRecord {
            name: sound.name.clone(),
            kind: media.kind,
            extension: sound.extension.clone(),
            filename: media.filename.clone(),
            effects: sound.effects,
            volume: sound.volume as f32,
            pan: sound.pan as f32,
            group_or_preload: if sound.new_audio {
                i32::try_from(media.group).map_err(|_| WriteError::SizeOverflow {
                    field: "sound group index",
                    size: media.group as u64,
                })?
            } else {
                i32::from(sound.preload)
            },
            audio_id: media.audio_id,
        });
    }
    Ok(records)
}

fn write_sound(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    sound: &SoundRecord,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &sound.name)?;
    writer.write_i32(sound.kind)?;
    strings.write_reference(writer, &sound.extension)?;
    strings.write_reference(writer, &sound.filename)?;
    writer.write_i32(sound.effects)?;
    writer.write_f32(sound.volume)?;
    writer.write_f32(sound.pan)?;
    writer.write_i32(sound.group_or_preload)?;
    writer.write_i32(sound.audio_id)
}

fn write_path(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    path: &GamePath,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &path.name)?;
    writer.write_i32(path.kind)?;
    writer.write_bool(path.closed)?;
    writer.write_i32(path.precision)?;
    writer.write_u32(as_u32(path.points.len(), "path point count")?)?;
    for point in &path.points {
        writer.write_f32(point.x as f32)?;
        writer.write_f32(point.y as f32)?;
        writer.write_f32(point.speed as f32)?;
    }
    Ok(())
}

fn prepare_sprites(assets: &Assets) -> Result<Vec<SpriteRecord<'_>>, WriteError> {
    assets
        .sprites
        .par_iter()
        .map(|sprite| {
            if sprite.sprite_type != SpriteType::Bitmap {
                return Err(invalid_resource(format!(
                    "sprite {:?} ({}) uses unsupported {} data; only bitmap sprites are supported",
                    sprite.name,
                    sprite.source.display(),
                    sprite.sprite_type
                )));
            }
            let frames = sprite
                .frames
                .iter()
                .map(|frame| decode_alpha_frame(assets, &frame.source))
                .collect::<Result<Vec<_>, _>>()?;
            let masks = sprite_masks(sprite, &frames)?;
            Ok(SpriteRecord { sprite, masks })
        })
        .collect()
}

fn decode_alpha_frame(assets: &Assets, source: &Path) -> Result<AlphaFrame, WriteError> {
    let bytes = assets.binary(source).ok_or_else(|| {
        invalid_resource(format!("sprite frame {} was not loaded", source.display()))
    })?;
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|error| invalid_resource(format!("{}: {error}", source.display())))?
        .into_rgba8();
    let width = i32::try_from(image.width()).map_err(|_| WriteError::SizeOverflow {
        field: "sprite frame width",
        size: u64::from(image.width()),
    })?;
    let height = i32::try_from(image.height()).map_err(|_| WriteError::SizeOverflow {
        field: "sprite frame height",
        size: u64::from(image.height()),
    })?;
    let alpha = image.pixels().map(|pixel| pixel[3]).collect();
    Ok(AlphaFrame {
        width,
        height,
        alpha,
    })
}

fn sprite_masks(sprite: &Sprite, frames: &[AlphaFrame]) -> Result<Vec<Vec<u8>>, WriteError> {
    if sprite.separate_masks {
        return frames
            .iter()
            .map(|frame| {
                let mut mask = empty_mask(sprite)?;
                apply_mask(sprite, frame, &mut mask, false);
                Ok(mask)
            })
            .collect();
    }

    let mut mask = empty_mask(sprite)?;
    for (index, frame) in frames.iter().enumerate() {
        apply_mask(sprite, frame, &mut mask, index != 0);
    }
    Ok(vec![mask])
}

fn empty_mask(sprite: &Sprite) -> Result<Vec<u8>, WriteError> {
    if sprite.width <= 0 || sprite.height <= 0 {
        return Ok(Vec::new());
    }
    let stride = usize::try_from((sprite.width + 7) / 8)
        .map_err(|_| invalid_resource(format!("sprite {} has an invalid width", sprite.name)))?;
    let height = usize::try_from(sprite.height)
        .map_err(|_| invalid_resource(format!("sprite {} has an invalid height", sprite.name)))?;
    let length = stride
        .checked_mul(height)
        .ok_or_else(|| invalid_resource(format!("sprite {} mask is too large", sprite.name)))?;
    Ok(vec![0; length])
}

fn apply_mask(sprite: &Sprite, frame: &AlphaFrame, mask: &mut [u8], merging: bool) {
    if sprite.width <= 0 || sprite.height <= 0 || frame.width <= 0 || frame.height <= 0 {
        return;
    }
    let (top, left, right, bottom) = mask_bounds(sprite, frame);
    if top > bottom || left > right {
        return;
    }
    let stride = ((sprite.width + 7) / 8) as usize;
    let set = |mask: &mut [u8], x: i32, y: i32| {
        let index = y as usize * stride + x as usize / 8;
        if let Some(byte) = mask.get_mut(index) {
            *byte |= 1 << (7 - (x & 7));
        }
    };

    match sprite.collision_kind {
        0 => {
            for y in top..=bottom {
                for x in left..=right {
                    let pixel = y as usize * frame.width as usize + x as usize;
                    if frame
                        .alpha
                        .get(pixel)
                        .is_some_and(|alpha| i32::from(*alpha) > sprite.collision_tolerance)
                    {
                        set(mask, x, y);
                    }
                }
            }
        }
        1 => {
            for y in top..=bottom {
                for x in left..=right {
                    set(mask, x, y);
                }
            }
        }
        2 => {
            let center_x = (sprite.bounding_box_left + sprite.bounding_box_right) / 2;
            let center_y = (sprite.bounding_box_bottom + sprite.bounding_box_top) / 2;
            let radius_x = (center_x - sprite.bounding_box_left) as f32 + 0.5;
            let radius_y = (center_y - sprite.bounding_box_top) as f32 + 0.5;
            for y in top..=bottom {
                for x in left..=right {
                    let dx = ((x as f32 - center_x as f32) / radius_x) as f64;
                    let dy = ((y as f32 - center_y as f32) / radius_y) as f64;
                    if radius_x > 0.0 && radius_y > 0.0 && dx.powi(2) + dy.powi(2) <= 1.0 {
                        set(mask, x, y);
                    }
                }
            }
        }
        3 => {
            let center_x = (sprite.bounding_box_left + sprite.bounding_box_right) / 2;
            let center_y = (sprite.bounding_box_bottom + sprite.bounding_box_top) / 2;
            let radius_x = (center_x - sprite.bounding_box_left) as f32 + 0.5;
            let radius_y = (center_y - sprite.bounding_box_top) as f32 + 0.5;
            for y in top..=bottom {
                for x in left..=right {
                    let dx = ((x as f32 - center_x as f32) / radius_x).abs();
                    let dy = ((y as f32 - center_y as f32) / radius_y).abs();
                    if radius_x > 0.0 && radius_y > 0.0 && dx + dy <= 1.0 {
                        set(mask, x, y);
                    }
                }
            }
        }
        5 if !merging => {
            for y in top..=bottom {
                for x in left..=right {
                    set(mask, x, y);
                }
            }
        }
        _ => {}
    }
}

fn mask_bounds(sprite: &Sprite, frame: &AlphaFrame) -> (i32, i32, i32, i32) {
    let (mut top, mut left, mut right, mut bottom) = match sprite.bounding_box_mode {
        0 => {
            let mut top = sprite.height - 1;
            let mut bottom = 0;
            let mut left = sprite.width - 1;
            let mut right = 0;
            for y in 0..frame.height {
                for x in 0..frame.width {
                    let pixel = y as usize * frame.width as usize + x as usize;
                    if frame
                        .alpha
                        .get(pixel)
                        .is_some_and(|alpha| i32::from(*alpha) > sprite.collision_tolerance)
                    {
                        left = left.min(x);
                        right = right.max(x);
                        top = top.min(y);
                        bottom = bottom.max(y);
                    }
                }
            }
            (top, left, right, bottom)
        }
        1 => (0, 0, sprite.width - 1, sprite.height - 1),
        _ => (
            sprite.bounding_box_top.clamp(0, sprite.height - 1),
            sprite.bounding_box_left.clamp(0, sprite.width - 1),
            sprite.bounding_box_right.clamp(0, sprite.width - 1),
            sprite.bounding_box_bottom.clamp(0, sprite.height - 1),
        ),
    };
    top = top.max(0);
    left = left.max(0);
    right = right.min(sprite.width - 1).min(frame.width - 1);
    bottom = bottom.min(sprite.height - 1).min(frame.height - 1);
    (top, left, right, bottom)
}

fn write_sprites(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    sprites: &[SpriteRecord<'_>],
    textures: &TextureData,
) -> Result<(), WriteError> {
    writer.write_u32(as_u32(sprites.len(), "sprite count")?)?;
    let mut patches = Vec::with_capacity(sprites.len());
    for _ in sprites {
        patches.push(writer.reserve_u32()?);
    }
    for (sprite, patch) in sprites.iter().zip(patches) {
        writer.align(4)?;
        writer.patch_position(patch)?;
        write_sprite(writer, strings, sprite, textures)?;
    }
    Ok(())
}

fn write_sprite(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    record: &SpriteRecord<'_>,
    textures: &TextureData,
) -> Result<(), WriteError> {
    let sprite = record.sprite;
    strings.write_reference(writer, &sprite.name)?;
    writer.write_i32(sprite.width)?;
    writer.write_i32(sprite.height)?;
    writer.write_i32(sprite.bounding_box_left)?;
    writer.write_i32(sprite.bounding_box_right)?;
    writer.write_i32(sprite.bounding_box_bottom)?;
    writer.write_i32(sprite.bounding_box_top)?;
    writer.write_bool(false)?;
    writer.write_bool(false)?;
    writer.write_bool(false)?;
    writer.write_i32(sprite.bounding_box_mode)?;
    let collision = match sprite.collision_type {
        CollisionType::AxisAlignedRectangle => 0,
        CollisionType::Precise => 1,
        CollisionType::RotatedRectangle => 2,
    };
    writer.write_i32(collision)?;
    writer.write_i32(sprite.x_origin)?;
    writer.write_i32(sprite.y_origin)?;
    writer.write_u32(as_u32(sprite.frames.len(), "sprite frame count")?)?;
    for (frame, _) in sprite.frames.iter().enumerate() {
        textures.write_sprite_reference(writer, sprite.index, frame)?;
    }
    writer.write_u32(as_u32(record.masks.len(), "sprite mask count")?)?;
    for mask in &record.masks {
        writer.write_all(mask)?;
    }
    Ok(())
}

fn write_background(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    background: &Background,
    textures: &TextureData,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &background.name)?;
    writer.write_bool(false)?;
    writer.write_bool(false)?;
    writer.write_bool(false)?;
    textures.write_background_reference(writer, background.index)
}

fn write_fonts(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    fonts: &[Font],
    textures: &TextureData,
) -> Result<(), WriteError> {
    writer.write_offset_table(fonts, |writer, font| {
        write_font(writer, strings, font, textures)
    })?;
    for value in 0_u16..=255 {
        writer.write_u16(if value < 128 { value } else { 63 })?;
    }
    Ok(())
}

fn write_font(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    font: &Font,
    textures: &TextureData,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &font.name)?;
    strings.write_reference(writer, &font.font_name)?;
    writer.write_i32(font.size)?;
    writer.write_bool(font.bold)?;
    writer.write_bool(font.italic)?;
    let first = font.first | font.charset.wrapping_shl(16) | font.anti_alias.wrapping_shl(24);
    writer.write_i32(first)?;
    writer.write_i32(font.last)?;
    textures.write_font_reference(writer, font.index)?;
    let (scale_x, scale_y) = textures.font_scale(font.index);
    writer.write_f32(scale_x)?;
    writer.write_f32(scale_y)?;
    writer.write_offset_table(&font.glyphs, |writer, glyph| {
        writer.write_i16(glyph.character as i16)?;
        writer.write_i16(glyph.x as i16)?;
        writer.write_i16(glyph.y as i16)?;
        writer.write_i16(glyph.width as i16)?;
        writer.write_i16(glyph.height as i16)?;
        writer.write_i16(glyph.shift as i16)?;
        writer.write_i16(glyph.offset as i16)?;
        writer.write_i16(glyph.kerning.len() as i16)?;
        for pair in &glyph.kerning {
            writer.write_i16(pair.other as i16)?;
            writer.write_i16(pair.amount as i16)?;
        }
        Ok(())
    })
}

fn script_records(compiled: &CompiledProject) -> Result<Vec<ScriptRecord>, WriteError> {
    compiled
        .codes
        .iter()
        .enumerate()
        .filter(|(_, code)| {
            matches!(
                code.kind,
                CodeKind::Script | CodeKind::GeneratedScript { .. } | CodeKind::Extension
            )
        })
        .map(|(index, code)| {
            let name = code
                .vm_name
                .strip_prefix("gml_Script_")
                .ok_or_else(|| {
                    invalid_resource(format!(
                        "script CODE entry {} has an invalid VM name",
                        code.vm_name
                    ))
                })?
                .to_owned();
            let code_index = i32::try_from(index).map_err(|_| WriteError::SizeOverflow {
                field: "script CODE index",
                size: index as u64,
            })?;
            Ok(ScriptRecord { name, code_index })
        })
        .collect()
}

impl CodeIndices {
    fn new(compiled: &CompiledProject) -> Result<Self, WriteError> {
        let mut result = Self::default();
        for (index, code) in compiled.codes.iter().enumerate() {
            let index = i32::try_from(index).map_err(|_| WriteError::SizeOverflow {
                field: "resource CODE index",
                size: index as u64,
            })?;
            let map = match code.kind {
                CodeKind::ObjectEvent => &mut result.object_events,
                CodeKind::Timeline => &mut result.timeline_entries,
                CodeKind::RoomCreation => &mut result.rooms,
                CodeKind::RoomInstance => &mut result.room_instances,
                _ => continue,
            };
            if map.insert(code.name.clone(), index).is_some() {
                return Err(invalid_resource(format!(
                    "duplicate {} CODE key {}",
                    code.kind, code.name
                )));
            }
        }
        Ok(result)
    }

    fn object_event(&self, object: &GameObject, event: &ObjectEvent) -> i32 {
        let key = format!("{}[{},{}]", object.name, event.event_type, event.subtype);
        self.object_events.get(&key).copied().unwrap_or(-1)
    }

    fn timeline_entry(&self, timeline: &Timeline, entry: &TimelineEntry) -> i32 {
        let key = format!("{}[step={}]", timeline.name, entry.step);
        self.timeline_entries.get(&key).copied().unwrap_or(-1)
    }

    fn room(&self, room: &Room) -> i32 {
        self.rooms.get(&room.name).copied().unwrap_or(-1)
    }

    fn room_instance(&self, room: &Room, instance: &RoomInstance) -> i32 {
        let key = format!("{}.{}", room.name, instance.name);
        self.room_instances.get(&key).copied().unwrap_or(-1)
    }
}

fn write_timeline(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    timeline: &Timeline,
    code: &CodeIndices,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &timeline.name)?;
    writer.write_u32(as_u32(timeline.entries.len(), "timeline entry count")?)?;
    let mut event_patches = Vec::with_capacity(timeline.entries.len());
    for entry in &timeline.entries {
        writer.write_i32(entry.step)?;
        event_patches.push(writer.reserve_u32()?);
    }
    for (entry, patch) in timeline.entries.iter().zip(event_patches) {
        writer.patch_position(patch)?;
        write_event_actions(
            writer,
            strings,
            &entry.actions,
            code.timeline_entry(timeline, entry),
        )?;
    }
    Ok(())
}

fn write_object(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    object: &GameObject,
    code: &CodeIndices,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &object.name)?;
    writer.write_i32(object.sprite_index)?;
    writer.write_bool(object.visible)?;
    writer.write_bool(object.solid)?;
    writer.write_i32(object.depth)?;
    writer.write_bool(object.persistent)?;
    writer.write_i32(object.parent_index)?;
    writer.write_i32(object.mask_index)?;
    writer.write_bool(object.physics_object)?;
    writer.write_bool(object.physics_sensor)?;
    writer.write_i32(object.physics_shape)?;
    writer.write_f32(object.physics_density)?;
    writer.write_f32(object.physics_restitution)?;
    writer.write_i32(object.physics_group)?;
    writer.write_f32(object.physics_linear_damping)?;
    writer.write_f32(object.physics_angular_damping)?;
    writer.write_u32(as_u32(
        object.physics_shape_points.len(),
        "physics vertex count",
    )?)?;
    writer.write_f32(object.physics_friction)?;
    writer.write_bool(object.physics_awake)?;
    writer.write_bool(object.physics_kinematic)?;
    for point in &object.physics_shape_points {
        writer.write_f32(point.x)?;
        writer.write_f32(point.y)?;
    }
    writer.write_offset_table(&object.events, |writer, events| {
        writer.write_offset_table(events, |writer, event| {
            writer.write_i32(event.subtype)?;
            write_event_actions(
                writer,
                strings,
                &event.actions,
                code.object_event(object, event),
            )
        })
    })
}

fn write_event_actions(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    source_actions: &[Action],
    code_index: i32,
) -> Result<(), WriteError> {
    if source_actions.is_empty() {
        return writer.write_u32(0);
    }

    // RemoveDND collapses every event to one canonical execute-code action.
    writer.write_u32(1)?;
    let action = writer.reserve_u32()?;
    writer.patch_position(action)?;
    writer.write_i32(1)?;
    writer.write_i32(source_actions[0].id)?;
    writer.write_i32(7)?;
    writer.write_bool(false)?;
    writer.write_bool(false)?;
    writer.write_bool(true)?;
    writer.write_i32(2)?;
    strings.write_reference(writer, "")?;
    writer.write_i32(code_index)?;
    writer.write_i32(1)?;
    writer.write_i32(-1)?;
    writer.write_bool(false)?;
    writer.write_bool(false)?;
    writer.write_i32(0)
}

fn write_shader(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    shader: &ShaderData,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &shader.name)?;
    writer.write_i32(shader.kind)?;
    strings.write_reference(writer, &shader.glsles_vertex)?;
    strings.write_reference(writer, &shader.glsles_fragment)?;
    strings.write_reference(writer, &shader.glsl_vertex)?;
    strings.write_reference(writer, &shader.glsl_fragment)?;
    strings.write_reference(writer, &shader.hlsl9_vertex)?;
    strings.write_reference(writer, &shader.hlsl9_pixel)?;

    let hlsl11_vertex = writer.reserve_u32()?;
    let hlsl11_pixel = writer.reserve_u32()?;
    writer.write_u32(as_u32(shader.attributes.len(), "shader attribute count")?)?;
    for attribute in &shader.attributes {
        strings.write_reference(writer, attribute)?;
    }
    writer.write_i32(2)?;

    // PSSL, CG/PS Vita and CG/PS3 vertex/pixel pointer-length pairs.
    for _ in 0..6 {
        writer.write_u32(0)?;
        writer.write_u32(0)?;
    }
    if !shader.hlsl11_vertex.is_empty() {
        writer.align(8)?;
        writer.patch_position(hlsl11_vertex)?;
        writer.write_all(&shader.hlsl11_vertex)?;
    }
    if !shader.hlsl11_pixel.is_empty() {
        writer.align(8)?;
        writer.patch_position(hlsl11_pixel)?;
        writer.write_all(&shader.hlsl11_pixel)?;
    }
    Ok(())
}

fn write_room(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    room: &Room,
    code: &CodeIndices,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &room.name)?;
    strings.write_reference(writer, &room.caption)?;
    writer.write_i32(room.width)?;
    writer.write_i32(room.height)?;
    writer.write_i32(room.speed)?;
    writer.write_bool(room.persistent)?;
    writer.write_i32(room.color)?;
    writer.write_bool(room.show_color)?;
    writer.write_i32(code.room(room))?;
    let flags = i32::from(room.enable_views)
        | (i32::from(room.clear_view_background) << 1)
        | (i32::from(!room.clear_display_buffer) << 2);
    writer.write_i32(flags)?;
    let backgrounds = writer.reserve_u32()?;
    let views = writer.reserve_u32()?;
    let instances = writer.reserve_u32()?;
    let tiles = writer.reserve_u32()?;
    writer.write_bool(room.physics_world)?;
    writer.write_i32(room.physics_world_top)?;
    writer.write_i32(room.physics_world_left)?;
    writer.write_i32(room.physics_world_right)?;
    writer.write_i32(room.physics_world_bottom)?;
    writer.write_f32(room.physics_world_gravity_x)?;
    writer.write_f32(room.physics_world_gravity_y)?;
    writer.write_f32(room.physics_world_pixels_to_meters)?;

    writer.patch_position(backgrounds)?;
    writer.write_offset_table(&room.backgrounds, write_room_background)?;
    writer.patch_position(views)?;
    writer.write_offset_table(&room.views, write_room_view)?;
    writer.patch_position(instances)?;
    writer.write_offset_table(&room.instances, |writer, instance| {
        write_room_instance(writer, room, instance, code)
    })?;
    writer.patch_position(tiles)?;
    writer.write_offset_table(&room.tiles, write_room_tile)
}

fn write_room_background(
    writer: &mut ChunkWriter<'_>,
    background: &RoomBackground,
) -> Result<(), WriteError> {
    writer.write_bool(background.visible)?;
    writer.write_bool(background.foreground)?;
    writer.write_i32(background.background_index)?;
    writer.write_i32(background.x)?;
    writer.write_i32(background.y)?;
    writer.write_bool(background.horizontal_tile)?;
    writer.write_bool(background.vertical_tile)?;
    writer.write_i32(background.horizontal_speed)?;
    writer.write_i32(background.vertical_speed)?;
    writer.write_bool(background.stretch)
}

fn write_room_view(writer: &mut ChunkWriter<'_>, view: &RoomView) -> Result<(), WriteError> {
    writer.write_bool(view.visible)?;
    writer.write_i32(view.x_view)?;
    writer.write_i32(view.y_view)?;
    writer.write_i32(view.width_view)?;
    writer.write_i32(view.height_view)?;
    writer.write_i32(view.x_port)?;
    writer.write_i32(view.y_port)?;
    writer.write_i32(view.width_port)?;
    writer.write_i32(view.height_port)?;
    writer.write_i32(view.horizontal_border)?;
    writer.write_i32(view.vertical_border)?;
    writer.write_i32(view.horizontal_speed)?;
    writer.write_i32(view.vertical_speed)?;
    writer.write_i32(view.object_index)
}

fn write_room_instance(
    writer: &mut ChunkWriter<'_>,
    room: &Room,
    instance: &RoomInstance,
    code: &CodeIndices,
) -> Result<(), WriteError> {
    writer.write_i32(instance.x)?;
    writer.write_i32(instance.y)?;
    writer.write_i32(instance.object_index)?;
    writer.write_i32(instance.id)?;
    writer.write_i32(code.room_instance(room, instance))?;
    writer.write_f32(instance.scale_x as f32)?;
    writer.write_f32(instance.scale_y as f32)?;
    writer.write_i32(instance.wad_color() as i32)?;
    writer.write_f32(instance.rotation as f32)?;
    // GMS 1.4 GMX has no instance pre-create code field.
    writer.write_i32(-1)
}

fn write_room_tile(writer: &mut ChunkWriter<'_>, tile: &RoomTile) -> Result<(), WriteError> {
    writer.write_i32(tile.x)?;
    writer.write_i32(tile.y)?;
    writer.write_i32(tile.background_index)?;
    writer.write_i32(tile.source_x)?;
    writer.write_i32(tile.source_y)?;
    writer.write_i32(tile.width)?;
    writer.write_i32(tile.height)?;
    writer.write_i32(tile.depth)?;
    writer.write_i32(tile.id)?;
    writer.write_f32(tile.scale_x as f32)?;
    writer.write_f32(tile.scale_y as f32)?;
    let alpha = (tile.alpha * 255.0) as i32;
    writer.write_i32(tile.blend.wrapping_add(alpha.wrapping_shl(24)))
}

fn as_u32(value: usize, field: &'static str) -> Result<u32, WriteError> {
    u32::try_from(value).map_err(|_| WriteError::SizeOverflow {
        field,
        size: value as u64,
    })
}

fn invalid_resource(message: impl Into<String>) -> WriteError {
    WriteError::InvalidVmData {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    fn sprite() -> Sprite {
        Sprite {
            index: 0,
            name: "mask".to_owned(),
            source: PathBuf::from("mask.sprite.gmx"),
            sprite_type: SpriteType::Bitmap,
            width: 5,
            height: 2,
            x_origin: 0,
            y_origin: 0,
            collision_kind: 0,
            collision_type: CollisionType::Precise,
            collision_tolerance: 0,
            separate_masks: false,
            bounding_box_mode: 1,
            bounding_box_left: 0,
            bounding_box_right: 4,
            bounding_box_top: 0,
            bounding_box_bottom: 1,
            horizontal_tile: false,
            vertical_tile: false,
            texture_groups: Vec::new(),
            for_3d: false,
            frames: Vec::new(),
            swf_source: None,
            swf_precision: 0.5,
            spine_source: None,
            playback_speed: 0.0,
            playback_speed_type: 1,
        }
    }

    #[test]
    fn merges_precise_masks_most_significant_bit_first() {
        let frames = [
            AlphaFrame {
                width: 5,
                height: 2,
                alpha: vec![255, 0, 255, 0, 255, 0, 0, 0, 0, 0],
            },
            AlphaFrame {
                width: 5,
                height: 2,
                alpha: vec![0, 255, 0, 255, 0, 0, 255, 0, 255, 0],
            },
        ];
        assert_eq!(
            sprite_masks(&sprite(), &frames).unwrap(),
            [vec![0xf8, 0x50]]
        );
    }

    #[test]
    fn writes_aligned_hlsl11_shader_blobs() {
        let strings = StringTable::new();
        let shader = ShaderData {
            name: "binary".to_owned(),
            kind: i32::MIN + 4,
            glsles_vertex: String::new(),
            glsles_fragment: String::new(),
            glsl_vertex: String::new(),
            glsl_fragment: String::new(),
            hlsl9_vertex: String::new(),
            hlsl9_pixel: String::new(),
            hlsl11_vertex: vec![1, 2, 3, 4, 5],
            hlsl11_pixel: vec![6, 7, 8],
            attributes: Vec::new(),
        };
        let mut builder = WadBuilder::new();
        builder
            .add_chunk(SHDR, |writer| {
                writer.write_offset_table(std::slice::from_ref(&shader), |writer, shader| {
                    write_shader(writer, &strings, shader)
                })
            })
            .unwrap();
        builder
            .add_chunk(FourCc::new(*b"STRG"), |writer| strings.write_strg(writer))
            .unwrap();
        let mut output = Cursor::new(Vec::new());
        let wad = builder.write_to(&mut output).unwrap();
        let bytes = output.into_inner();
        let chunk = wad.chunks.iter().find(|chunk| chunk.name == SHDR).unwrap();
        let record = read_test_u32(&bytes, chunk.data_offset as usize + 4) as usize;
        let vertex = read_test_u32(&bytes, record + 32) as usize;
        let pixel = read_test_u32(&bytes, record + 36) as usize;
        assert_eq!(vertex % 8, 0);
        assert_eq!(pixel % 8, 0);
        assert_eq!(&bytes[vertex..vertex + 5], [1, 2, 3, 4, 5]);
        assert_eq!(&bytes[pixel..pixel + 3], [6, 7, 8]);
    }

    fn read_test_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }
}
