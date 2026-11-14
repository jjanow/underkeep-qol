//! GL context info and a save/restore scope for every piece of GL state the
//! overlay (egui_glow) touches, so the game's state is unchanged after the
//! menu draws.

use egui_glow::glow::{self, HasContext};

/// Not in glow's constant list under these names on every version.
const CONTEXT_PROFILE_MASK: u32 = 0x9126;
const CONTEXT_CORE_PROFILE_BIT: i32 = 0x1;
const CONTEXT_COMPATIBILITY_PROFILE_BIT: i32 = 0x2;

#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub major: u32,
    pub gles: bool,
    pub vao: bool,
    pub samplers: bool,
    pub srgb_fb: bool,
    pub polygon_mode: bool,
}

/// Logs the context's version and profile; returns what state exists.
pub fn caps(gl: &glow::Context) -> Caps {
    let v = gl.version();
    let (major, minor, gles) = (v.major, v.minor, v.is_embedded);
    let at_least = |a: u32, b: u32| (major, minor) >= (a, b);
    // SAFETY: plain queries on the current context.
    let (version, renderer, vendor, glsl, profile) = unsafe {
        let profile = if !gles && at_least(3, 2) {
            match gl.get_parameter_i32(CONTEXT_PROFILE_MASK) {
                m if m & CONTEXT_CORE_PROFILE_BIT != 0 => "core",
                m if m & CONTEXT_COMPATIBILITY_PROFILE_BIT != 0 => "compatibility",
                _ => "unknown",
            }
        } else if gles {
            "ES"
        } else {
            "legacy (no profiles before 3.2)"
        };
        (
            gl.get_parameter_string(glow::VERSION),
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VENDOR),
            gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION),
            profile,
        )
    };
    crate::log(&format!(
        "overlay: GL {major}.{minor} {profile} profile; GL_VERSION \"{version}\", GLSL \"{glsl}\", {renderer} ({vendor})"
    ));
    let ext = gl.supported_extensions();
    Caps {
        major,

        gles,
        vao: at_least(3, 0) || ext.contains("GL_ARB_vertex_array_object"),
        samplers: at_least(3, 3) || ext.contains("GL_ARB_sampler_objects"),
        srgb_fb: !gles && (at_least(3, 0) || ext.contains("GL_ARB_framebuffer_sRGB")),
        polygon_mode: !gles,
    }
}

fn nz(v: i32) -> Option<std::num::NonZeroU32> {
    std::num::NonZeroU32::new(v as u32)
}

/// The game's GL state, restored on drop (also when the overlay panics).
pub struct Saved<'a> {
    gl: &'a glow::Context,
    caps: Caps,
    program: i32,
    vao: i32,
    vao0_element: i32,
    array_buffer: i32,
    element_buffer: i32,
    active_texture: i32,
    texture0: i32,
    sampler0: i32,
    unpack_buffer: i32,
    unpack: [i32; 4],
    draw_fb: i32,
    read_fb: i32,
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    color_mask: [bool; 4],
    blend_func: [i32; 4],
    blend_eq: [i32; 2],
    polygon_mode: i32,
    enabled: [(u32, bool); 7],
}

const UNPACK: [u32; 4] = [
    glow::UNPACK_ALIGNMENT,
    glow::UNPACK_ROW_LENGTH,
    glow::UNPACK_SKIP_PIXELS,
    glow::UNPACK_SKIP_ROWS,
];

impl<'a> Saved<'a> {
    /// Saves the state and leaves the context ready for egui: drawing to
    /// the default framebuffer, texture unit 0 with no sampler, no unpack
    /// buffer, no stencil test, filled polygons.
    ///
    /// # Safety
    /// `gl` must be the current context on this thread.
    pub unsafe fn save(gl: &'a glow::Context, caps: Caps) -> Saved<'a> {
        let i = |p| unsafe { gl.get_parameter_i32(p) };
        let caps_list = [
            glow::BLEND,
            glow::SCISSOR_TEST,
            glow::DEPTH_TEST,
            glow::CULL_FACE,
            glow::STENCIL_TEST,
            glow::RASTERIZER_DISCARD,
            glow::FRAMEBUFFER_SRGB,
        ];
        let mut enabled = caps_list.map(|c| (c, false));
        for e in &mut enabled {
            if e.0 == glow::FRAMEBUFFER_SRGB && !caps.srgb_fb {
                continue;
            }
            e.1 = unsafe { gl.is_enabled(e.0) };
        }
        let mut viewport = [0; 4];
        let mut scissor_box = [0; 4];
        let mask;
        let mut poly = [glow::FILL as i32; 2];
        unsafe {
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
            mask = gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK);
            if caps.polygon_mode {
                gl.get_parameter_i32_slice(glow::POLYGON_MODE, &mut poly[..1]);
            }
        }
        let vao = if caps.vao {
            i(glow::VERTEX_ARRAY_BINDING)
        } else {
            0
        };
        let element_buffer = i(glow::ELEMENT_ARRAY_BUFFER_BINDING);
        // egui unbinds ELEMENT_ARRAY_BUFFER with VAO 0 bound when it's done,
        // which changes VAO 0's binding; remember it.
        let vao0_element = if caps.vao && vao != 0 {
            unsafe {
                gl.bind_vertex_array(None);
                let e = i(glow::ELEMENT_ARRAY_BUFFER_BINDING);
                gl.bind_vertex_array(nz(vao).map(glow::NativeVertexArray));
                e
            }
        } else {
            element_buffer
        };
        let active_texture = i(glow::ACTIVE_TEXTURE);
        unsafe { gl.active_texture(glow::TEXTURE0) };
        let s = Saved {
            gl,
            caps,
            program: i(glow::CURRENT_PROGRAM),
            vao,
            vao0_element,
            array_buffer: i(glow::ARRAY_BUFFER_BINDING),
            element_buffer,
            active_texture,
            texture0: i(glow::TEXTURE_BINDING_2D),
            sampler0: if caps.samplers {
                i(glow::SAMPLER_BINDING)
            } else {
                0
            },
            unpack_buffer: if caps.major >= 3 || caps.gles {
                i(glow::PIXEL_UNPACK_BUFFER_BINDING)
            } else {
                0
            },
            unpack: UNPACK.map(i),
            draw_fb: i(glow::DRAW_FRAMEBUFFER_BINDING),
            read_fb: i(glow::READ_FRAMEBUFFER_BINDING),
            viewport,
            scissor_box,
            color_mask: mask,
            blend_func: [
                i(glow::BLEND_SRC_RGB),
                i(glow::BLEND_DST_RGB),
                i(glow::BLEND_SRC_ALPHA),
                i(glow::BLEND_DST_ALPHA),
            ],
            blend_eq: [i(glow::BLEND_EQUATION_RGB), i(glow::BLEND_EQUATION_ALPHA)],
            polygon_mode: poly[0],
            enabled,
        };
        // State egui doesn't set itself but depends on.
        unsafe {
            if caps.samplers {
                gl.bind_sampler(0, None);
            }
            if s.unpack_buffer != 0 {
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            }
            for p in &UNPACK[1..] {
                gl.pixel_store_i32(*p, 0);
            }
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::RASTERIZER_DISCARD);
            if caps.polygon_mode {
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
            }
        }
        s
    }
}

impl Drop for Saved<'_> {
    fn drop(&mut self) {
        let gl = self.gl;
        let c = self.caps;
        // SAFETY: same context as `save` (dropped in the same frame).
        unsafe {
            gl.use_program(nz(self.program).map(glow::NativeProgram));
            if c.vao {
                gl.bind_vertex_array(None);
                gl.bind_buffer(
                    glow::ELEMENT_ARRAY_BUFFER,
                    nz(self.vao0_element).map(glow::NativeBuffer),
                );
                gl.bind_vertex_array(nz(self.vao).map(glow::NativeVertexArray));
            } else {
                gl.bind_buffer(
                    glow::ELEMENT_ARRAY_BUFFER,
                    nz(self.element_buffer).map(glow::NativeBuffer),
                );
            }
            gl.bind_buffer(
                glow::ARRAY_BUFFER,
                nz(self.array_buffer).map(glow::NativeBuffer),
            );
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, nz(self.texture0).map(glow::NativeTexture));
            if c.samplers {
                gl.bind_sampler(0, nz(self.sampler0).map(glow::NativeSampler));
            }
            gl.active_texture(self.active_texture as u32);
            if c.major >= 3 || c.gles {
                gl.bind_buffer(
                    glow::PIXEL_UNPACK_BUFFER,
                    nz(self.unpack_buffer).map(glow::NativeBuffer),
                );
            }
            for (p, v) in UNPACK.iter().zip(self.unpack) {
                gl.pixel_store_i32(*p, v);
            }
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                nz(self.draw_fb).map(glow::NativeFramebuffer),
            );
            gl.bind_framebuffer(
                glow::READ_FRAMEBUFFER,
                nz(self.read_fb).map(glow::NativeFramebuffer),
            );
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            let [x, y, w, h] = self.scissor_box;
            gl.scissor(x, y, w, h);
            let [r, g, b, a] = self.color_mask;
            gl.color_mask(r, g, b, a);
            let [sr, dr, sa, da] = self.blend_func.map(|v| v as u32);
            gl.blend_func_separate(sr, dr, sa, da);
            gl.blend_equation_separate(self.blend_eq[0] as u32, self.blend_eq[1] as u32);
            if c.polygon_mode {
                gl.polygon_mode(glow::FRONT_AND_BACK, self.polygon_mode as u32);
            }
            for (cap, on) in self.enabled {
                if cap == glow::FRAMEBUFFER_SRGB && !c.srgb_fb {
                    continue;
                }
                if on {
                    gl.enable(cap);
                } else {
                    gl.disable(cap);
                }
            }
        }
    }
}
