use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use image::codecs::png::PngEncoder;
use image::imageops::{FilterType, crop_imm, resize};
use image::{ImageEncoder, RgbaImage};
use md5::{Digest, Md5};
use rayon::prelude::*;

use crate::assets::Assets;
use crate::cache::{cache_enabled, write_atomic};
use crate::resources::{Background, Font, Sprite, SpriteType};
use crate::settings::TextureGroupSettings;
use crate::wad::{ChunkOptions, ChunkWriter, FourCc, WadBuilder, WriteError};

const TPAG: FourCc = FourCc::new(*b"TPAG");
const TXTR: FourCc = FourCc::new(*b"TXTR");
const TEXTURE_CACHE_SCHEMA: &[u8] = b"gmx-rs-texture-cache-v1\0";
const CRC_TABLE: [u32; 256] = crc_table();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryOwner {
    Sprite { sprite: usize, frame: usize },
    Background(usize),
    Font(usize),
}

#[derive(Debug, Clone)]
struct GroupSpec {
    name: String,
    scaled: bool,
    border: i32,
    remove_space: bool,
    target_mask: i64,
    parent: Option<String>,
    mips_to_generate: i32,
}

impl From<&TextureGroupSettings> for GroupSpec {
    fn from(group: &TextureGroupSettings) -> Self {
        Self {
            name: group.name.clone(),
            scaled: group.scaled,
            border: group.border,
            remove_space: group.remove_space,
            target_mask: group.target_mask,
            parent: group.parent.clone(),
            mips_to_generate: group.mips_to_generate,
        }
    }
}

#[derive(Debug, Clone)]
struct ImageRequest {
    owner: EntryOwner,
    source: std::path::PathBuf,
    sort_name: String,
    group: GroupSpec,
    pack: bool,
    original_repeat_border: bool,
    tile_h: bool,
    tile_v: bool,
    leave_border_empty: bool,
}

#[derive(Debug)]
struct WorkingEntry {
    owner: EntryOwner,
    sort_name: String,
    group: GroupSpec,
    image: RgbaImage,
    hash: u32,
    x: i32,
    y: i32,
    x_offset: i32,
    y_offset: i32,
    crop_width: i32,
    crop_height: i32,
    original_width: i32,
    original_height: i32,
    original_repeat_border: bool,
    tile_h: bool,
    tile_v: bool,
    leave_border_empty: bool,
    same_as: Option<usize>,
    page: usize,
}

impl WorkingEntry {
    fn width(&self) -> i32 {
        self.image.width() as i32
    }

    fn height(&self) -> i32 {
        self.image.height() as i32
    }

    fn duplicate_key_matches(&self, other: &Self) -> bool {
        self.width() == other.width()
            && self.height() == other.height()
            && self.hash == other.hash
            && self.group.name == other.group.name
            && self.x_offset == other.x_offset
            && self.y_offset == other.y_offset
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TexturePageEntry {
    x: i16,
    y: i16,
    width: i16,
    height: i16,
    x_offset: i16,
    y_offset: i16,
    crop_width: i16,
    crop_height: i16,
    original_width: i16,
    original_height: i16,
    page: i16,
}

#[derive(Debug)]
struct TexturePage {
    scaled: i32,
    mips_to_generate: i32,
    png: Vec<u8>,
}

#[derive(Debug)]
pub struct TextureData {
    entries: Vec<TexturePageEntry>,
    pages: Vec<TexturePage>,
    sprite_entries: Vec<Vec<Option<usize>>>,
    background_entries: Vec<Option<usize>>,
    font_entries: Vec<Option<usize>>,
    font_scales: Vec<(f32, f32)>,
    references: RefCell<Vec<Vec<u64>>>,
}

impl TextureData {
    pub fn prepare(assets: &Assets, cache_root: &Path) -> Result<Self, WriteError> {
        let page_size = assets.settings.texture_page_size;
        let requests = image_requests(assets)?;
        let mut working = requests
            .par_iter()
            .map(|request| decode_request(assets, request, page_size))
            .collect::<Result<Vec<_>, _>>()?;

        working.sort_by(|left, right| {
            culture_cmp(&left.group.name, &right.group.name)
                .then_with(|| (right.width() * right.height()).cmp(&(left.width() * left.height())))
                .then_with(|| culture_cmp(&left.sort_name, &right.sort_name))
        });

        let mut groups = Vec::<Vec<WorkingEntry>>::new();
        for entry in working {
            if groups
                .last()
                .is_none_or(|group| group[0].group.name != entry.group.name)
            {
                groups.push(Vec::new());
            }
            groups.last_mut().unwrap().push(entry);
        }
        let packed_groups = groups
            .par_iter_mut()
            .map(|group| {
                let mut pages = Vec::new();
                let len = group.len();
                mark_duplicates(group, 0, len);
                pack_group(group, 0, len, page_size, &mut pages)?;
                Ok(pages)
            })
            .collect::<Result<Vec<_>, WriteError>>()?;

        let entry_count = groups.iter().map(Vec::len).sum();
        let mut working = Vec::with_capacity(entry_count);
        let mut page_builds = Vec::<PageBuild>::new();
        for (mut group, pages) in groups.into_iter().zip(packed_groups) {
            let entry_base = working.len();
            let page_base = page_builds.len();
            for entry in &mut group {
                entry.same_as = entry.same_as.map(|same| entry_base + same);
                entry.page += page_base;
            }
            for mut page in pages {
                page.entries
                    .iter_mut()
                    .for_each(|entry| *entry += entry_base);
                page_builds.push(page);
            }
            working.append(&mut group);
        }
        resolve_duplicates(&mut working);

        let pages = page_builds
            .par_iter()
            .map(|page| encode_page(page, &working, cache_root))
            .collect::<Result<Vec<_>, _>>()?;

        let mut sprite_entries = assets
            .sprites
            .iter()
            .map(|sprite| vec![None; sprite.frames.len()])
            .collect::<Vec<_>>();
        let mut background_entries = vec![None; assets.backgrounds.len()];
        let mut font_entries = vec![None; assets.fonts.len()];
        let mut font_scales = vec![(1.0, 1.0); assets.fonts.len()];
        for (entry_index, entry) in working.iter().enumerate() {
            match entry.owner {
                EntryOwner::Sprite { sprite, frame } => {
                    sprite_entries[sprite][frame] = Some(entry_index);
                }
                EntryOwner::Background(index) => background_entries[index] = Some(entry_index),
                EntryOwner::Font(index) => {
                    font_entries[index] = Some(entry_index);
                    font_scales[index] = (
                        entry.original_width as f32 / entry.width() as f32,
                        entry.original_height as f32 / entry.height() as f32,
                    );
                }
            }
        }

        let entries = working
            .iter()
            .map(|entry| {
                Ok(TexturePageEntry {
                    x: as_i16(entry.x, "texture page x")?,
                    y: as_i16(entry.y, "texture page y")?,
                    width: as_i16(entry.width(), "texture page width")?,
                    height: as_i16(entry.height(), "texture page height")?,
                    x_offset: as_i16(entry.x_offset, "texture x offset")?,
                    y_offset: as_i16(entry.y_offset, "texture y offset")?,
                    crop_width: as_i16(entry.crop_width, "texture crop width")?,
                    crop_height: as_i16(entry.crop_height, "texture crop height")?,
                    original_width: as_i16(entry.original_width, "texture original width")?,
                    original_height: as_i16(entry.original_height, "texture original height")?,
                    page: as_i16(entry.page as i32, "texture page index")?,
                })
            })
            .collect::<Result<Vec<_>, WriteError>>()?;
        let references = RefCell::new(vec![Vec::new(); entries.len()]);

        Ok(Self {
            entries,
            pages,
            sprite_entries,
            background_entries,
            font_entries,
            font_scales,
            references,
        })
    }

    pub fn write_sprite_reference(
        &self,
        writer: &mut ChunkWriter<'_>,
        sprite: usize,
        frame: usize,
    ) -> Result<(), WriteError> {
        self.write_reference(writer, self.sprite_entries[sprite][frame])
    }

    pub fn write_background_reference(
        &self,
        writer: &mut ChunkWriter<'_>,
        background: usize,
    ) -> Result<(), WriteError> {
        self.write_reference(writer, self.background_entries[background])
    }

    pub fn write_font_reference(
        &self,
        writer: &mut ChunkWriter<'_>,
        font: usize,
    ) -> Result<(), WriteError> {
        self.write_reference(writer, self.font_entries[font])
    }

    pub fn font_scale(&self, font: usize) -> (f32, f32) {
        self.font_scales[font]
    }

    fn write_reference(
        &self,
        writer: &mut ChunkWriter<'_>,
        entry: Option<usize>,
    ) -> Result<(), WriteError> {
        let Some(entry) = entry else {
            return writer.write_u32(0);
        };
        let offset = writer.position()?;
        writer.write_u32(0)?;
        self.references.borrow_mut()[entry].push(offset);
        Ok(())
    }

    fn write_tpag(&self, writer: &mut ChunkWriter<'_>) -> Result<(), WriteError> {
        writer.write_u32(as_u32(self.entries.len(), "texture page entry count")?)?;
        let mut records = Vec::with_capacity(self.entries.len());
        for _ in &self.entries {
            records.push(writer.reserve_u32()?);
        }
        let mut references = self.references.borrow_mut();
        for (index, (entry, record)) in self.entries.iter().zip(records).enumerate() {
            writer.patch_position(record)?;
            let offset = writer.position_u32()?;
            for reference in references[index].drain(..) {
                writer.patch_u32_at(reference, offset)?;
            }
            writer.write_i16(entry.x)?;
            writer.write_i16(entry.y)?;
            writer.write_i16(entry.width)?;
            writer.write_i16(entry.height)?;
            writer.write_i16(entry.x_offset)?;
            writer.write_i16(entry.y_offset)?;
            writer.write_i16(entry.crop_width)?;
            writer.write_i16(entry.crop_height)?;
            writer.write_i16(entry.original_width)?;
            writer.write_i16(entry.original_height)?;
            writer.write_i16(entry.page)?;
        }
        Ok(())
    }

    fn write_txtr(&self, writer: &mut ChunkWriter<'_>) -> Result<(), WriteError> {
        writer.write_u32(as_u32(self.pages.len(), "texture count")?)?;
        let mut records = Vec::with_capacity(self.pages.len());
        for _ in &self.pages {
            records.push(writer.reserve_u32()?);
        }
        let mut data_patches = Vec::with_capacity(self.pages.len());
        for (page, record) in self.pages.iter().zip(records) {
            writer.patch_position(record)?;
            writer.write_i32(page.scaled)?;
            data_patches.push(writer.reserve_u32()?);
            let _ = page.mips_to_generate;
        }
        for (page, patch) in self.pages.iter().zip(data_patches) {
            writer.align(128)?;
            writer.patch_position(patch)?;
            writer.write_all(&page.png)?;
        }
        Ok(())
    }
}

pub fn add_texture_chunks<'a>(
    builder: &mut WadBuilder<'a>,
    textures: &'a TextureData,
) -> Result<(), WriteError> {
    builder.add_chunk(TPAG, move |writer| textures.write_tpag(writer))?;
    builder.add_chunk_with(TXTR, ChunkOptions::TEXTURE, move |writer| {
        textures.write_txtr(writer)
    })?;
    Ok(())
}

fn image_requests(assets: &Assets) -> Result<Vec<ImageRequest>, WriteError> {
    let configured = assets
        .settings
        .texture_groups
        .iter()
        .map(GroupSpec::from)
        .collect::<Vec<_>>();
    let group_by_name = configured
        .iter()
        .enumerate()
        .map(|(index, group)| (group.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut requests = Vec::new();
    let mut unique_group = 0;

    for sprite in &assets.sprites {
        if sprite.sprite_type != SpriteType::Bitmap {
            return Err(invalid_texture(format!(
                "sprite {:?} ({}) uses unsupported {} texture data; only bitmap sprites are supported",
                sprite.name,
                sprite.source.display(),
                sprite.sprite_type
            )));
        }
        for (frame, image) in sprite.frames.iter().enumerate() {
            let group = if sprite.for_3d {
                GroupSpec {
                    name: format!(
                        "__YY__{unique_group}{}_YYG_AUTO_GEN_TEX_GROUP_NAME_{frame}",
                        sprite.name
                    ),
                    scaled: false,
                    border: 0,
                    remove_space: false,
                    target_mask: i64::MAX,
                    parent: None,
                    mips_to_generate: 0,
                }
            } else {
                selected_group(
                    &configured,
                    &group_by_name,
                    selected_group_index(&sprite.texture_groups, assets.config_index),
                    assets.settings.target_mask,
                )?
            };
            if group.target_mask & assets.settings.target_mask == 0 {
                continue;
            }
            requests.push(sprite_request(sprite, frame, &image.source, group));
        }
        if sprite.for_3d {
            unique_group += 1;
        }
    }
    for background in &assets.backgrounds {
        let group = if background.for_3d {
            let result = GroupSpec {
                name: format!("__YY__{}{unique_group}", background.name),
                scaled: false,
                border: 0,
                remove_space: false,
                target_mask: i64::MAX,
                parent: None,
                mips_to_generate: 0,
            };
            unique_group += 1;
            result
        } else {
            selected_group(
                &configured,
                &group_by_name,
                selected_group_index(&background.texture_groups, assets.config_index),
                assets.settings.target_mask,
            )?
        };
        if group.target_mask & assets.settings.target_mask != 0
            && assets.binary(&background.image_source).is_some()
        {
            requests.push(background_request(background, group));
        }
    }
    for font in &assets.fonts {
        let group = selected_group(
            &configured,
            &group_by_name,
            selected_group_index(&font.texture_groups, assets.config_index),
            assets.settings.target_mask,
        )?;
        if group.target_mask & assets.settings.target_mask != 0 {
            requests.push(font_request(font, group));
        }
    }
    Ok(requests)
}

fn selected_group_index(groups: &[i32], config: usize) -> usize {
    groups
        .get(config)
        .or_else(|| groups.first())
        .and_then(|value| usize::try_from(*value).ok())
        .unwrap_or(0)
}

fn selected_group(
    groups: &[GroupSpec],
    by_name: &HashMap<String, usize>,
    index: usize,
    target_mask: i64,
) -> Result<GroupSpec, WriteError> {
    let initial = groups
        .get(index)
        .or_else(|| groups.first())
        .ok_or_else(|| invalid_texture("no texture groups are configured"))?;
    if initial.target_mask & target_mask == 0 {
        return Ok(initial.clone());
    }
    let mut group = initial;
    let mut visited = Vec::<String>::new();
    while let Some(parent) = &group.parent {
        if visited.iter().any(|name| name == &group.name) {
            return Err(invalid_texture(format!(
                "texture group parent cycle contains {}",
                group.name
            )));
        }
        visited.push(group.name.clone());
        let Some(parent_index) = by_name.get(parent) else {
            break;
        };
        group = &groups[*parent_index];
    }
    Ok(group.clone())
}

fn sprite_request(sprite: &Sprite, frame: usize, source: &Path, group: GroupSpec) -> ImageRequest {
    ImageRequest {
        owner: EntryOwner::Sprite {
            sprite: sprite.index,
            frame,
        },
        source: source.to_path_buf(),
        sort_name: cache_name(&sprite.name, source),
        group,
        pack: true,
        original_repeat_border: false,
        tile_h: sprite.horizontal_tile,
        tile_v: sprite.vertical_tile,
        leave_border_empty: false,
    }
}

fn background_request(background: &Background, group: GroupSpec) -> ImageRequest {
    ImageRequest {
        owner: EntryOwner::Background(background.index),
        source: background.image_source.clone(),
        sort_name: cache_name(&background.name, &background.image_source),
        group,
        pack: !background.tileset,
        original_repeat_border: true,
        tile_h: background.horizontal_tile,
        tile_v: background.vertical_tile,
        leave_border_empty: false,
    }
}

fn font_request(font: &Font, group: GroupSpec) -> ImageRequest {
    ImageRequest {
        owner: EntryOwner::Font(font.index),
        source: font.image_source.clone(),
        sort_name: cache_name(&font.name, &font.image_source),
        group,
        pack: false,
        original_repeat_border: false,
        tile_h: false,
        tile_v: false,
        leave_border_empty: true,
    }
}

fn cache_name(resource: &str, source: &Path) -> String {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    format!("{resource}_{stem}.tpe.xml")
}

fn decode_request(
    assets: &Assets,
    request: &ImageRequest,
    page_size: i32,
) -> Result<WorkingEntry, WriteError> {
    let bytes = assets.binary(&request.source).ok_or_else(|| {
        invalid_texture(format!(
            "texture source {} was not loaded",
            request.source.display()
        ))
    })?;
    let mut image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|error| invalid_texture(format!("{}: {error}", request.source.display())))?
        .into_rgba8();
    let original_width = as_i32(image.width(), "texture source width")?;
    let original_height = as_i32(image.height(), "texture source height")?;
    let (mut x_offset, mut y_offset, mut crop_width, mut crop_height) =
        (0, 0, original_width, original_height);

    if request.pack && request.group.remove_space {
        let bounds = alpha_bounds(&image);
        match bounds {
            None => {
                crop_width = 1;
                crop_height = 1;
            }
            Some((left, top, right, bottom)) => {
                x_offset = left as i32;
                y_offset = top as i32;
                crop_width = (right - left + 1) as i32;
                crop_height = (bottom - top + 1) as i32;
                if left != 0
                    || top != 0
                    || right + 1 != image.width()
                    || bottom + 1 != image.height()
                {
                    image =
                        crop_imm(&image, left, top, right - left + 1, bottom - top + 1).to_image();
                }
            }
        }
    }

    if image.width() > page_size as u32 || image.height() > page_size as u32 {
        let mut width = image.width();
        let mut height = image.height();
        while width > page_size as u32 || height > page_size as u32 {
            width = (width / 2).max(1);
            height = (height / 2).max(1);
        }
        image = resize(&image, width, height, FilterType::CatmullRom);
    }
    let hash = bitmap_crc(&image);
    Ok(WorkingEntry {
        owner: request.owner,
        sort_name: request.sort_name.clone(),
        group: request.group.clone(),
        image,
        hash,
        x: 0,
        y: 0,
        x_offset,
        y_offset,
        crop_width,
        crop_height,
        original_width,
        original_height,
        original_repeat_border: request.original_repeat_border,
        tile_h: request.tile_h,
        tile_v: request.tile_v,
        leave_border_empty: request.leave_border_empty,
        same_as: None,
        page: 0,
    })
}

fn alpha_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let mut left = image.width();
    let mut top = image.height();
    let mut right = 0;
    let mut bottom = 0;
    let mut found = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] != 0 {
            found = true;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    found.then_some((left, top, right, bottom))
}

fn bitmap_crc(image: &RgbaImage) -> u32 {
    let mut crc = u32::MAX;
    for pixel in image.pixels() {
        for byte in [pixel[2], pixel[1], pixel[0], pixel[3]] {
            crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize];
        }
    }
    crc
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0;
    while index < table.len() {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(value & 1));
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn mark_duplicates(entries: &mut [WorkingEntry], start: usize, end: usize) {
    let mut candidates = HashMap::<(i32, i32, u32, i32, i32), Vec<usize>>::new();
    for index in (start..end).rev() {
        let entry = &entries[index];
        let key = (
            entry.width(),
            entry.height(),
            entry.hash,
            entry.x_offset,
            entry.y_offset,
        );
        let duplicate = candidates.get(&key).and_then(|indices| {
            indices
                .iter()
                .copied()
                .find(|candidate| entries[*candidate].image == entries[index].image)
        });
        if let Some(duplicate) = duplicate {
            debug_assert!(entries[index].duplicate_key_matches(&entries[duplicate]));
            entries[index].same_as = Some(duplicate);
        } else {
            candidates.entry(key).or_default().push(index);
        }
    }
}

#[derive(Debug)]
struct PageBuild {
    width: i32,
    height: i32,
    scaled: i32,
    mips_to_generate: i32,
    entries: Vec<usize>,
}

fn pack_group(
    entries: &mut [WorkingEntry],
    start: usize,
    end: usize,
    page_size: i32,
    output: &mut Vec<PageBuild>,
) -> Result<(), WriteError> {
    let page_base = output.len();
    let border = entries[start].group.border;
    let mut pages = Vec::<PagePacking>::new();
    for (entry_index, entry) in entries.iter_mut().enumerate().take(end).skip(start) {
        if entry.same_as.is_some() {
            continue;
        }
        let mut placed = None;
        for (page_index, page) in pages.iter_mut().enumerate() {
            if let Some((x, y)) = page.place(entry, border) {
                placed = Some((page_index, x, y));
                break;
            }
        }
        if placed.is_none() {
            let mut page = PagePacking::new(page_size, page_size);
            let (x, y) = page.place(entry, border).ok_or_else(|| {
                invalid_texture(format!(
                    "texture {}x{} does not fit a {page_size}x{page_size} page",
                    entry.width(),
                    entry.height()
                ))
            })?;
            pages.push(page);
            placed = Some((pages.len() - 1, x, y));
        }
        let (page, x, y) = placed.unwrap();
        entry.x = x;
        entry.y = y;
        entry.page = page_base + page;
        pages[page].entries.push(entry_index);
    }

    let layouts = pages
        .par_iter()
        .map(|page| shrink_page(entries, &page.entries, page_size, border))
        .collect::<Result<Vec<_>, _>>()?;
    for (local_page, (page, layout)) in pages.iter().zip(layouts).enumerate() {
        for (entry_index, (x, y)) in page.entries.iter().copied().zip(layout.positions) {
            entries[entry_index].x = x;
            entries[entry_index].y = y;
            entries[entry_index].page = page_base + local_page;
        }
        output.push(PageBuild {
            width: layout.width,
            height: layout.height,
            scaled: i32::from(entries[start].group.scaled),
            mips_to_generate: entries[start].group.mips_to_generate,
            entries: page.entries.clone(),
        });
    }
    Ok(())
}

struct PageLayout {
    width: i32,
    height: i32,
    positions: Vec<(i32, i32)>,
}

fn shrink_page(
    entries: &[WorkingEntry],
    page_entries: &[usize],
    page_size: i32,
    border: i32,
) -> Result<PageLayout, WriteError> {
    let mut width = page_size;
    let mut height = page_size;
    loop {
        let mut next_width = width;
        let mut next_height = height;
        if width > 1 && pack_dimensions(entries, page_entries, width / 2, height, border).is_some()
        {
            next_width = width / 2;
        }
        if height > 1
            && pack_dimensions(entries, page_entries, next_width, height / 2, border).is_some()
        {
            next_height = height / 2;
        }
        if next_width == width && next_height == height {
            break;
        }
        width = next_width;
        height = next_height;
    }
    let positions = pack_dimensions(entries, page_entries, width, height, border)
        .ok_or_else(|| invalid_texture("texture page could not be repacked after shrinking"))?;
    Ok(PageLayout {
        width,
        height,
        positions,
    })
}

fn pack_dimensions(
    entries: &[WorkingEntry],
    page_entries: &[usize],
    width: i32,
    height: i32,
    border: i32,
) -> Option<Vec<(i32, i32)>> {
    let mut page = PagePacking::new(width, height);
    let mut result = Vec::with_capacity(page_entries.len());
    for index in page_entries {
        result.push(page.place(&entries[*index], border)?);
    }
    Some(result)
}

#[derive(Debug)]
struct PagePacking {
    width: i32,
    height: i32,
    skyline: Skyline,
    entries: Vec<usize>,
}

impl PagePacking {
    fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            skyline: Skyline::new(width, height),
            entries: Vec::new(),
        }
    }

    fn place(&mut self, entry: &WorkingEntry, border: i32) -> Option<(i32, i32)> {
        let mut gap_x = border;
        let mut gap_y = border;
        if entry.original_repeat_border {
            gap_x += border;
            gap_y += border;
        }
        let mut width = entry.width();
        let mut height = entry.height();
        let mut inset_x = 0;
        let mut inset_y = 0;
        if width + gap_x * 2 < self.width {
            width = round_four(width + gap_x * 2);
            inset_x = gap_x;
        }
        if height + gap_y * 2 < self.height {
            height = round_four(height + gap_y * 2);
            inset_y = gap_y;
        }
        let candidate = self.skyline.find(width, height)?;
        let (x, y) = self.skyline.place(candidate, width, height);
        Some((x + inset_x, y + inset_y))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SkylineNode {
    x: i32,
    y: i32,
    width: i32,
}

#[derive(Debug)]
struct Skyline {
    width: i32,
    height: i32,
    nodes: Vec<SkylineNode>,
}

impl Skyline {
    fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            nodes: vec![SkylineNode { x: 0, y: 0, width }],
        }
    }

    fn find(&self, width: i32, height: i32) -> Option<usize> {
        let mut best = None::<(i32, i32, usize)>;
        for (index, node) in self.nodes.iter().enumerate() {
            let Some(y) = self.fit(index, width, height) else {
                continue;
            };
            let score = (y + height, node.x, index);
            if best.is_none_or(|current| score < current) {
                best = Some(score);
            }
        }
        best.map(|(_, _, index)| index)
    }

    fn place(&mut self, candidate: usize, width: i32, height: i32) -> (i32, i32) {
        let x = self.nodes[candidate].x;
        let y = self
            .fit(candidate, width, height)
            .expect("skyline candidate must remain valid until placement");
        let right = x + width;
        self.nodes.insert(
            candidate,
            SkylineNode {
                x,
                y: y + height,
                width,
            },
        );

        let index = candidate + 1;
        while index < self.nodes.len() {
            let overlap = right - self.nodes[index].x;
            if overlap <= 0 {
                break;
            }
            if overlap < self.nodes[index].width {
                self.nodes[index].x += overlap;
                self.nodes[index].width -= overlap;
                break;
            }
            self.nodes.remove(index);
        }

        let mut index = 1;
        while index < self.nodes.len() {
            if self.nodes[index - 1].y == self.nodes[index].y {
                self.nodes[index - 1].width += self.nodes[index].width;
                self.nodes.remove(index);
            } else {
                index += 1;
            }
        }
        (x, y)
    }

    fn fit(&self, start: usize, width: i32, height: i32) -> Option<i32> {
        let x = self.nodes.get(start)?.x;
        if width <= 0 || height <= 0 || x + width > self.width {
            return None;
        }
        let mut remaining = width;
        let mut y = 0;
        let mut index = start;
        while remaining > 0 {
            let node = self.nodes.get(index)?;
            y = y.max(node.y);
            if y + height > self.height {
                return None;
            }
            remaining -= node.width;
            index += 1;
        }
        Some(y)
    }
}

fn resolve_duplicates(entries: &mut [WorkingEntry]) {
    for index in 0..entries.len() {
        let Some(mut same) = entries[index].same_as else {
            continue;
        };
        while let Some(next) = entries[same].same_as {
            same = next;
        }
        entries[index].x = entries[same].x;
        entries[index].y = entries[same].y;
        entries[index].page = entries[same].page;
    }
}

fn encode_page(
    page: &PageBuild,
    entries: &[WorkingEntry],
    cache_root: &Path,
) -> Result<TexturePage, WriteError> {
    let cache_path = cache_enabled().then(|| {
        cache_root
            .join("texture")
            .join(format!("{}.png", page_cache_key(page, entries)))
    });
    if let Some(path) = &cache_path
        && let Ok(png) = fs::read(path)
        && png.len() > 8
        && png.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        return Ok(TexturePage {
            scaled: page.scaled,
            mips_to_generate: page.mips_to_generate,
            png,
        });
    }
    let mut atlas = RgbaImage::new(page.width as u32, page.height as u32);
    for index in &page.entries {
        let entry = &entries[*index];
        blit_entry(&mut atlas, entry);
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            atlas.as_raw(),
            atlas.width(),
            atlas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| invalid_texture(format!("cannot encode texture page: {error}")))?;
    if let Some(path) = cache_path {
        let _ = write_atomic(&path, &png);
    }
    Ok(TexturePage {
        scaled: page.scaled,
        mips_to_generate: page.mips_to_generate,
        png,
    })
}

fn page_cache_key(page: &PageBuild, entries: &[WorkingEntry]) -> String {
    let mut hasher = Md5::new();
    hasher.update(TEXTURE_CACHE_SCHEMA);
    hasher.update(page.width.to_le_bytes());
    hasher.update(page.height.to_le_bytes());
    hasher.update(page.scaled.to_le_bytes());
    hasher.update(page.mips_to_generate.to_le_bytes());
    for index in &page.entries {
        let entry = &entries[*index];
        for value in [
            entry.x,
            entry.y,
            entry.x_offset,
            entry.y_offset,
            entry.crop_width,
            entry.crop_height,
            entry.original_width,
            entry.original_height,
            entry.group.border,
        ] {
            hasher.update(value.to_le_bytes());
        }
        hasher.update([
            u8::from(entry.original_repeat_border),
            u8::from(entry.tile_h),
            u8::from(entry.tile_v),
            u8::from(entry.leave_border_empty),
        ]);
        hasher.update(entry.image.width().to_le_bytes());
        hasher.update(entry.image.height().to_le_bytes());
        hasher.update(entry.image.as_raw());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn blit_entry(atlas: &mut RgbaImage, entry: &WorkingEntry) {
    for (x, y, pixel) in entry.image.enumerate_pixels() {
        let target_x = entry.x + x as i32;
        let target_y = entry.y + y as i32;
        if target_x >= 0
            && target_y >= 0
            && target_x < atlas.width() as i32
            && target_y < atlas.height() as i32
        {
            atlas.put_pixel(target_x as u32, target_y as u32, *pixel);
        }
    }
    let border = entry.group.border;
    if border <= 0 || entry.leave_border_empty {
        return;
    }
    let width = entry.width();
    let height = entry.height();
    let border_x = if entry.x + width + border * 2 < atlas.width() as i32 {
        border
    } else {
        0
    };
    let border_y = if entry.y + height + border * 2 < atlas.height() as i32 {
        border
    } else {
        0
    };
    for y in -border_y..height + border_y {
        for x in -border_x..width + border_x {
            if x >= 0 && x < width && y >= 0 && y < height {
                continue;
            }
            let target_x = entry.x + x;
            let target_y = entry.y + y;
            if target_x < 0
                || target_y < 0
                || target_x >= atlas.width() as i32
                || target_y >= atlas.height() as i32
            {
                continue;
            }
            let mut valid = true;
            let source_x = if entry.tile_h {
                x.rem_euclid(width)
            } else if x < 0 {
                if entry.x_offset > 0 {
                    valid = false;
                }
                0
            } else if x >= width {
                if entry.x_offset + entry.crop_width < entry.original_width {
                    valid = false;
                }
                width - 1
            } else {
                x
            };
            let source_y = if entry.tile_v {
                y.rem_euclid(height)
            } else if y < 0 {
                if entry.y_offset > 0 {
                    valid = false;
                }
                0
            } else if y >= height {
                if entry.y_offset + entry.crop_height < entry.original_height {
                    valid = false;
                }
                height - 1
            } else {
                y
            };
            if valid {
                let pixel = *entry.image.get_pixel(source_x as u32, source_y as u32);
                atlas.put_pixel(target_x as u32, target_y as u32, pixel);
            }
        }
    }
}

fn culture_cmp(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(collation_unit)
        .cmp(right.bytes().map(collation_unit))
        .then_with(|| {
            left.bytes()
                .map(|byte| u8::from(byte.is_ascii_uppercase()))
                .cmp(
                    right
                        .bytes()
                        .map(|byte| u8::from(byte.is_ascii_uppercase())),
                )
        })
        .then_with(|| left.cmp(right))
}

fn collation_unit(byte: u8) -> (u8, u8) {
    if byte.is_ascii_alphabetic() {
        (2, byte.to_ascii_lowercase())
    } else if byte.is_ascii_digit() {
        (1, byte)
    } else {
        (0, byte)
    }
}

fn round_four(value: i32) -> i32 {
    (value + 3) & !3
}

fn as_i32(value: u32, field: &'static str) -> Result<i32, WriteError> {
    i32::try_from(value).map_err(|_| WriteError::SizeOverflow {
        field,
        size: u64::from(value),
    })
}

fn as_i16(value: i32, field: &'static str) -> Result<i16, WriteError> {
    i16::try_from(value).map_err(|_| WriteError::SizeOverflow {
        field,
        size: value.max(0) as u64,
    })
}

fn as_u32(value: usize, field: &'static str) -> Result<u32, WriteError> {
    u32::try_from(value).map_err(|_| WriteError::SizeOverflow {
        field,
        size: value as u64,
    })
}

fn invalid_texture(message: impl Into<String>) -> WriteError {
    WriteError::InvalidVmData {
        message: format!("texture data: {}", message.into()),
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{
        EntryOwner, GroupSpec, Skyline, WorkingEntry, alpha_bounds, bitmap_crc, mark_duplicates,
    };

    #[test]
    fn detects_nontransparent_bounds() {
        let mut image = RgbaImage::new(5, 4);
        image.put_pixel(1, 2, Rgba([10, 20, 30, 1]));
        image.put_pixel(3, 3, Rgba([40, 50, 60, 255]));
        assert_eq!(alpha_bounds(&image), Some((1, 2, 3, 3)));
        assert_eq!(alpha_bounds(&RgbaImage::new(2, 2)), None);
    }

    #[test]
    fn bitmap_crc_uses_gdi_bgra_byte_order_without_final_xor() {
        let image = RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 4]));
        assert_eq!(bitmap_crc(&image), 0xd1fc_ae3b);
    }

    #[test]
    fn duplicate_buckets_keep_the_last_exact_image_as_canonical() {
        let red = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let blue = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 255, 255]));
        let mut entries = vec![
            working_entry(red.clone()),
            working_entry(blue),
            working_entry(red.clone()),
            working_entry(red),
        ];
        entries.iter_mut().for_each(|entry| entry.hash = 42);
        let len = entries.len();

        mark_duplicates(&mut entries, 0, len);

        assert_eq!(entries[0].same_as, Some(3));
        assert_eq!(entries[1].same_as, None);
        assert_eq!(entries[2].same_as, Some(3));
        assert_eq!(entries[3].same_as, None);
    }

    #[test]
    fn skyline_places_deterministically() {
        let mut skyline = Skyline::new(32, 32);
        let first = skyline.find(8, 8).unwrap();
        assert_eq!(skyline.place(first, 8, 8), (0, 0));
        let second = skyline.find(8, 8).unwrap();
        assert_eq!(skyline.place(second, 8, 8), (8, 0));
    }

    #[test]
    fn skyline_keeps_mixed_rectangles_in_bounds_and_disjoint() {
        let mut state = 0x8fd5_31a7_u32;
        let mut pages = Vec::<(Skyline, Vec<(i32, i32, i32, i32)>)>::new();
        for _ in 0..512 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let width = 4 + i32::try_from(state % 61).unwrap();
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let height = 4 + i32::try_from(state % 61).unwrap();

            let mut placed = false;
            for (skyline, rectangles) in &mut pages {
                let Some(candidate) = skyline.find(width, height) else {
                    continue;
                };
                let (x, y) = skyline.place(candidate, width, height);
                assert_disjoint(rectangles, x, y, width, height);
                rectangles.push((x, y, width, height));
                placed = true;
                break;
            }
            if !placed {
                let mut skyline = Skyline::new(256, 256);
                let candidate = skyline.find(width, height).unwrap();
                let (x, y) = skyline.place(candidate, width, height);
                pages.push((skyline, vec![(x, y, width, height)]));
            }
        }

        assert!(pages.len() < 20);
    }

    fn assert_disjoint(
        rectangles: &[(i32, i32, i32, i32)],
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        assert!(x >= 0 && y >= 0 && x + width <= 256 && y + height <= 256);
        for &(other_x, other_y, other_width, other_height) in rectangles {
            assert!(
                x + width <= other_x
                    || other_x + other_width <= x
                    || y + height <= other_y
                    || other_y + other_height <= y
            );
        }
    }

    fn working_entry(image: RgbaImage) -> WorkingEntry {
        let width = image.width() as i32;
        let height = image.height() as i32;
        WorkingEntry {
            owner: EntryOwner::Background(0),
            sort_name: String::new(),
            group: GroupSpec {
                name: "Default".to_owned(),
                scaled: false,
                border: 2,
                remove_space: true,
                target_mask: i64::MAX,
                parent: None,
                mips_to_generate: 0,
            },
            hash: bitmap_crc(&image),
            image,
            x: 0,
            y: 0,
            x_offset: 0,
            y_offset: 0,
            crop_width: width,
            crop_height: height,
            original_width: width,
            original_height: height,
            original_repeat_border: false,
            tile_h: false,
            tile_v: false,
            leave_border_empty: false,
            same_as: None,
            page: 0,
        }
    }
}
