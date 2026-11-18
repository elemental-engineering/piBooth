//! OpenGL ES 3 renderer. Pass 1 draws the streamed RGB camera texture as a
//! letterboxed, optionally mirrored quad. Pass 2 alpha-blends OSD sprites
//! (pre-rendered RGBA textures) on top. All GL calls happen on the render
//! thread.

use anyhow::{Context, anyhow};
use glow::HasContext;

use crate::frame::Frame;
use crate::geometry::{letterbox, quad_vertices};
use crate::osd::{Overlay, Rgba, SpriteId};

const VERTEX_SHADER: &str = r#"#version 300 es
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const CAMERA_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 o_color;
void main() {
    o_color = vec4(texture(u_tex, v_uv).rgb, 1.0);
}
"#;

const OVERLAY_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_tint;
out vec4 o_color;
void main() {
    o_color = texture(u_tex, v_uv) * u_tint;
}
"#;

struct Sprite {
    texture: glow::Texture,
    size: (u32, u32),
}

pub struct Renderer {
    gl: glow::Context,
    camera_program: glow::Program,
    overlay_program: glow::Program,
    tint_loc: Option<glow::UniformLocation>,
    camera_vao: glow::VertexArray,
    camera_vbo: glow::Buffer,
    overlay_vao: glow::VertexArray,
    overlay_vbo: glow::Buffer,
    texture: glow::Texture,
    tex_size: Option<(u32, u32)>,
    sprites: Vec<Option<Sprite>>,
    screen: (u32, u32),
    mirror: bool,
    quad_dirty: bool,
}

impl Renderer {
    /// # Safety
    /// `gl` must be current on this thread for the renderer's whole life.
    pub unsafe fn new(gl: glow::Context, screen: (u32, u32), mirror: bool) -> anyhow::Result<Self> {
        unsafe {
            let camera_program = link_program(&gl, VERTEX_SHADER, CAMERA_FRAGMENT_SHADER)?;
            let overlay_program = link_program(&gl, VERTEX_SHADER, OVERLAY_FRAGMENT_SHADER)?;
            let (camera_vao, camera_vbo) = quad_buffers(&gl)?;
            let (overlay_vao, overlay_vbo) = quad_buffers(&gl)?;
            let texture = new_texture(&gl)?;
            // Camera rows are tightly packed RGB; 1456*3 is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

            for program in [camera_program, overlay_program] {
                gl.use_program(Some(program));
                let loc = gl.get_uniform_location(program, "u_tex");
                gl.uniform_1_i32(loc.as_ref(), 0);
            }
            let tint_loc = gl.get_uniform_location(overlay_program, "u_tint");
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);

            Ok(Self {
                gl,
                camera_program,
                overlay_program,
                tint_loc,
                camera_vao,
                camera_vbo,
                overlay_vao,
                overlay_vbo,
                texture,
                tex_size: None,
                sprites: Vec::new(),
                screen,
                mirror,
                quad_dirty: true,
            })
        }
    }

    pub fn resize(&mut self, screen: (u32, u32)) {
        if screen != self.screen {
            self.screen = screen;
            self.quad_dirty = true;
        }
    }

    pub fn screen(&self) -> (u32, u32) {
        self.screen
    }

    pub fn set_mirror(&mut self, mirror: bool) {
        if mirror != self.mirror {
            self.mirror = mirror;
            self.quad_dirty = true;
        }
    }

    pub fn mirror(&self) -> bool {
        self.mirror
    }

    /// Upload a new camera frame into the streamed texture.
    pub fn upload(&mut self, frame: &Frame) {
        assert_eq!(
            frame.data.len(),
            Frame::byte_len(frame.width, frame.height),
            "frame buffer length does not match its size"
        );
        let gl = &self.gl;
        let size = (frame.width, frame.height);
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            let pixels = glow::PixelUnpackData::Slice(Some(&frame.data));
            if self.tex_size != Some(size) {
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGB8 as i32,
                    size.0 as i32,
                    size.1 as i32,
                    0,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
                self.tex_size = Some(size);
                self.quad_dirty = true;
            } else {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    size.0 as i32,
                    size.1 as i32,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
            }
        }
    }

    /// Create a GPU texture from an RGBA image; draw it via [`Overlay`]s.
    pub fn create_sprite(&mut self, image: &Rgba) -> anyhow::Result<SpriteId> {
        let texture = unsafe { new_texture(&self.gl)? };
        let id = SpriteId(self.sprites.len());
        self.sprites.push(Some(Sprite { texture, size: (0, 0) }));
        self.update_sprite(id, image);
        Ok(id)
    }

    /// Replace a sprite's pixels (e.g. the debug text once a second).
    pub fn update_sprite(&mut self, id: SpriteId, image: &Rgba) {
        assert_eq!(image.data.len(), image.width as usize * image.height as usize * 4, "RGBA length mismatch");
        let Some(Some(sprite)) = self.sprites.get_mut(id.0) else { return };
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(sprite.texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                image.width as i32,
                image.height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&image.data)),
            );
        }
        sprite.size = (image.width, image.height);
    }

    pub fn delete_sprite(&mut self, id: SpriteId) {
        if let Some(slot) = self.sprites.get_mut(id.0)
            && let Some(sprite) = slot.take()
        {
            unsafe { self.gl.delete_texture(sprite.texture) };
        }
    }

    pub fn draw(&mut self, overlays: &[Overlay]) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, self.screen.0 as i32, self.screen.1 as i32);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.active_texture(glow::TEXTURE0);
            if let Some(tex_size) = self.tex_size {
                gl.use_program(Some(self.camera_program));
                gl.bind_vertex_array(Some(self.camera_vao));
                if self.quad_dirty {
                    let rect = letterbox(tex_size, self.screen);
                    let verts = quad_vertices(rect, self.screen, self.mirror);
                    gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.camera_vbo));
                    gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&verts));
                    self.quad_dirty = false;
                }
                gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            if overlays.is_empty() {
                return;
            }
            gl.enable(glow::BLEND);
            gl.use_program(Some(self.overlay_program));
            gl.bind_vertex_array(Some(self.overlay_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.overlay_vbo));
            for o in overlays {
                let Some(Some(sprite)) = self.sprites.get(o.sprite.0) else { continue };
                let verts = quad_vertices(o.rect, self.screen, false);
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&verts));
                gl.uniform_4_f32(self.tint_loc.as_ref(), o.tint[0], o.tint[1], o.tint[2], o.tint[3]);
                gl.bind_texture(glow::TEXTURE_2D, Some(sprite.texture));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            gl.disable(glow::BLEND);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            for sprite in self.sprites.drain(..).flatten() {
                self.gl.delete_texture(sprite.texture);
            }
            self.gl.delete_texture(self.texture);
            self.gl.delete_buffer(self.camera_vbo);
            self.gl.delete_vertex_array(self.camera_vao);
            self.gl.delete_buffer(self.overlay_vbo);
            self.gl.delete_vertex_array(self.overlay_vao);
            self.gl.delete_program(self.camera_program);
            self.gl.delete_program(self.overlay_program);
        }
    }
}

/// VAO + VBO for one 4-vertex `[x, y, u, v]` triangle strip.
unsafe fn quad_buffers(gl: &glow::Context) -> anyhow::Result<(glow::VertexArray, glow::Buffer)> {
    unsafe {
        let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
        let vbo = gl.create_buffer().map_err(|e| anyhow!(e))?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_size(glow::ARRAY_BUFFER, 16 * 4, glow::DYNAMIC_DRAW);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
        Ok((vao, vbo))
    }
}

unsafe fn new_texture(gl: &glow::Context) -> anyhow::Result<glow::Texture> {
    unsafe {
        let texture = gl.create_texture().map_err(|e| anyhow!(e))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (k, v) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
        }
        Ok(texture)
    }
}

fn as_bytes(v: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}

unsafe fn link_program(gl: &glow::Context, vs: &str, fs: &str) -> anyhow::Result<glow::Program> {
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow!(e))?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let s = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
            gl.shader_source(s, src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(anyhow!(gl.get_shader_info_log(s))).context("shader compile failed");
            }
            gl.attach_shader(program, s);
            shaders.push(s);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(anyhow!(gl.get_program_info_log(program))).context("program link failed");
        }
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        Ok(program)
    }
}
