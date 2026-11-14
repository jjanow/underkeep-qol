//! Screenshots on request, for testing without access to the screen (a
//! locked desktop, a remote session): create `uk_qol/screenshot.request`
//! and the next frame, overlay included, is written to
//! `uk_qol/screenshot.bmp` (24-bit, the window's pixel size); the request
//! file is then removed. Checked every 15th frame, so it costs nothing
//! otherwise.

use std::cell::Cell;
use std::path::Path;

use egui_glow::glow::{self, HasContext};

const EVERY: u32 = 15;

thread_local! {
    static COUNT: Cell<u32> = const { Cell::new(0) };
}

/// Call right before the game presents, with its context current.
pub(super) fn maybe(gl: &glow::Context, width: i32, height: i32) {
    let n = COUNT.with(|c| {
        let n = c.get().wrapping_add(1);
        c.set(n);
        n
    });
    if !n.is_multiple_of(EVERY) || width <= 0 || height <= 0 {
        return;
    }
    let Some(dir) = crate::data_dir() else { return };
    let request = dir.join("screenshot.request");
    if !request.exists() {
        return;
    }
    let _ = std::fs::remove_file(&request);
    let (w, h) = (width as usize, height as usize);
    let mut px = vec![0u8; w * h * 3];
    // SAFETY: the game's context is current; every state changed here is
    // restored before returning.
    unsafe {
        let read_fb = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
        let pack_fb = gl.get_parameter_i32(glow::PIXEL_PACK_BUFFER_BINDING);
        let align = gl.get_parameter_i32(glow::PACK_ALIGNMENT);
        let read_buf = gl.get_parameter_i32(glow::READ_BUFFER);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        gl.read_buffer(glow::BACK);
        gl.read_pixels(
            0,
            0,
            width,
            height,
            glow::BGR,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut px)),
        );
        gl.read_buffer(read_buf as u32);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, align);
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, gl_name(pack_fb));
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, gl_name(read_fb).map(framebuffer));
    }
    match write_bmp(&dir.join("screenshot.bmp"), w, h, &px) {
        Ok(()) => crate::log(&format!("screenshot written ({w}x{h})")),
        Err(e) => crate::log(&format!("screenshot failed: {e}")),
    }
}

fn gl_name(v: i32) -> Option<glow::NativeBuffer> {
    std::num::NonZeroU32::new(v as u32).map(glow::NativeBuffer)
}

fn framebuffer(b: glow::NativeBuffer) -> glow::NativeFramebuffer {
    glow::NativeFramebuffer(b.0)
}

/// Bottom-up BGR rows, as `glReadPixels` returns them, are BMP's own order.
fn write_bmp(path: &Path, w: usize, h: usize, bgr: &[u8]) -> std::io::Result<()> {
    let row = w * 3;
    let stride = row.div_ceil(4) * 4;
    let size = 54 + stride * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for r in bgr.chunks(row).take(h) {
        out.extend_from_slice(r);
        out.resize(out.len() + (stride - row), 0);
    }
    crate::config::write_atomic(path, &out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bmp_header_and_padding() {
        let dir = std::env::temp_dir().join(format!("uk-qol-bmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.bmp");
        super::write_bmp(&p, 2, 2, &[1; 12]).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[..2], b"BM");
        assert_eq!(b.len(), 54 + 8 * 2);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&dir);
    }
}
