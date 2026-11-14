//! The game's own sprites, read at run time from the player's install, so
//! mods can draw game art without shipping any.
//!
//! A sprite is looked up by its name in `base/_sprite.db` (texture name and
//! rect), its texture in `base/_texture.db` (image file), and cut out of
//! that image in `base/` (`dft::db` has the formats). Each sprite is
//! decoded once per session and cached, a failure too (logged once).
//! Mods draw it through [`texture`], one GPU texture per egui context
//! (the overlay makes a new context when the game's GL context changes).
//! Sprites use nearest filtering, like the game's pixel art.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dft::db::{self, Value};

/// A sprite cut out of its atlas: RGBA pixels and the pivot (in pixels from
/// the top-left).
#[derive(Clone)]
pub struct Sprite {
    pub image: Arc<egui::ColorImage>,
    pub pivot: [f32; 2],
}

impl Sprite {
    /// Size in pixels (one GUI unit each in the game's HUD art).
    pub fn size(&self) -> egui::Vec2 {
        egui::vec2(self.image.size[0] as f32, self.image.size[1] as f32)
    }
}

static CACHE: Mutex<BTreeMap<String, Option<Sprite>>> = Mutex::new(BTreeMap::new());

/// `<game folder>/base`.
fn base_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("base"))
}

/// The sprite `name` from the game's files, or `None` (logged on the first
/// try) if it can't be read.
pub fn get(name: &str) -> Option<Sprite> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = cache.get(name) {
        return s.clone();
    }
    let r = base_dir()
        .ok_or_else(|| "no game folder".to_string())
        .and_then(|b| load(&b, name));
    let s = match r {
        Ok(s) => {
            crate::log(&format!(
                "sprite {name}: {}x{} from the game's files",
                s.image.size[0], s.image.size[1]
            ));
            Some(s)
        }
        Err(e) => {
            crate::log(&format!("sprite {name}: can't read it ({e})"));
            None
        }
    };
    cache.insert(name.to_owned(), s.clone());
    s
}

/// A texture of sprite `name` for drawing with egui in `ctx` (made once per
/// context and kept in it), with the sprite's size in pixels.
pub fn texture(ctx: &egui::Context, name: &str) -> Option<(egui::TextureHandle, egui::Vec2)> {
    let id = egui::Id::unique(("uk_qol_sprite", name));
    if let Some(t) = ctx.data(|d| d.get_temp::<(egui::TextureHandle, egui::Vec2)>(id)) {
        return Some(t);
    }
    let s = get(name)?;
    let tex = ctx.load_texture(
        format!("sprite:{name}"),
        egui::ImageData::Color(s.image.clone()),
        egui::TextureOptions::NEAREST,
    );
    let t = (tex, s.size());
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

fn read_db(base: &Path, file: &str) -> Result<Value, String> {
    let d = std::fs::read(base.join(file)).map_err(|e| format!("{file}: {e}"))?;
    db::parse(&d).map_err(|e| format!("{file}: {e}"))
}

fn metadata<'a>(root: &'a Value, file: &str, name: &str) -> Result<&'a Value, String> {
    db::record(root, name)
        .and_then(|r| r.get("metadata"))
        .ok_or_else(|| format!("{file} has no {name}"))
}

fn floats<const N: usize>(v: Option<&Value>) -> Option<[f32; N]> {
    let a = v?.as_array()?;
    if a.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (o, v) in out.iter_mut().zip(a) {
        *o = v.as_f32()?;
    }
    Some(out)
}

/// Reads sprite `name` from the game folder's `base` directory.
pub fn load(base: &Path, name: &str) -> Result<Sprite, String> {
    let sprites = read_db(base, "_sprite.db")?;
    let m = metadata(&sprites, "_sprite.db", name)?;
    let texture = m
        .get("texture")
        .and_then(Value::as_str)
        .ok_or("no texture")?;
    let [x, y, w, h] = floats::<4>(m.get("rect")).ok_or("no rect")?;
    let pivot = floats::<2>(m.get("pivot")).unwrap_or([w / 2.0, h / 2.0]);
    let textures = read_db(base, "_texture.db")?;
    let file = metadata(&textures, "_texture.db", texture)?
        .get("file")
        .and_then(Value::as_str)
        .ok_or("no texture file")?;
    // Only a plain file name in `base`.
    if file.contains(['/', '\\']) || file.contains("..") || !file.ends_with(".png") {
        return Err(format!("texture file {file:?} isn't a PNG in base"));
    }
    let (iw, ih, rgba) = decode_png(&base.join(file)).map_err(|e| format!("{file}: {e}"))?;
    let rect = [x, y, w, h].map(|v| v.round() as i64);
    let [x, y, w, h] = rect;
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 || x < 0 || y < 0 || x + w > iw || y + h > ih {
        return Err(format!("rect {rect:?} outside {file} ({iw}x{ih})"));
    }
    let (x, y, w, h, iw) = (x as usize, y as usize, w as usize, h as usize, iw as usize);
    let mut px = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = (row * iw + x) * 4;
        px.extend_from_slice(&rgba[start..start + w * 4]);
    }
    Ok(Sprite {
        image: Arc::new(egui::ColorImage::from_rgba_unmultiplied([w, h], &px)),
        pivot,
    })
}

/// Decodes a PNG to 8-bit RGBA: (width, height, pixels).
fn decode_png(path: &Path) -> Result<(i64, i64, Vec<u8>), String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(f));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let size = reader.output_buffer_size().ok_or("image too large")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[g, a]| [g, g, g, a])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        c => return Err(format!("unsupported colour type {c:?}")),
    };
    Ok((i64::from(info.width), i64::from(info.height), rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `UNDERKEEP_DIR` set: the empty weapon slot's icon on the
    /// character sheet (the one weapon-swap draws) loads, with the size
    /// and the few colours of the game's slot silhouettes.
    #[test]
    fn installed_sword_icon() {
        let Some(dir) = std::env::var_os("UNDERKEEP_DIR") else {
            return;
        };
        let base = PathBuf::from(dir).join("base");
        let s = load(&base, "hud_barbie_sword").unwrap();
        assert_eq!(s.image.size, [21, 21]);
        let opaque: std::collections::BTreeSet<_> = s
            .image
            .pixels
            .iter()
            .filter(|p| p.a() == 255)
            .map(|p| (p.r(), p.g(), p.b()))
            .collect();
        assert!(opaque.contains(&(45, 45, 57)), "{opaque:?}");
        assert!(opaque.contains(&(15, 15, 27)), "{opaque:?}");
        assert!(load(&base, "no_such_sprite").is_err());
    }
}
