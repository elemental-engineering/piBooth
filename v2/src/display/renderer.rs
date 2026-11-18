//! OpenGL ES 3 renderer: one streamed RGB texture drawn as a letterboxed,
//! optionally mirrored quad. All GL calls happen on the render thread.

use anyhow::{Context, anyhow};
use glow::HasContext;

use crate::frame::Frame;
use crate::geometry::{letterbox, quad_vertices};

const VERTEX_SHADER: &str = r#"#version 300 es
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 o_color;
void main() {
    o_color = vec4(texture(u_tex, v_uv).rgb, 1.0);
}
"#;

pub struct Renderer {
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    texture: glow::Texture,
    tex_size: Option<(u32, u32)>,
    screen: (u32, u32),
    mirror: bool,
    quad_dirty: bool,
}

impl Renderer {
    /// # Safety
    /// `gl` must be current on this thread for the renderer's whole life.
    pub unsafe fn new(gl: glow::Context, screen: (u32, u32), mirror: bool) -> anyhow::Result<Self> {
        unsafe {
            let program = link_program(&gl, VERTEX_SHADER, FRAGMENT_SHADER)?;
            let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
            let vbo = gl.create_buffer().map_err(|e| anyhow!(e))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, 16 * 4, glow::DYNAMIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);

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
            // Rows are tightly packed RGB; 1456*3 is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

            gl.use_program(Some(program));
            let loc = gl.get_uniform_location(program, "u_tex");
            gl.uniform_1_i32(loc.as_ref(), 0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);

            Ok(Self { gl, program, vao, vbo, texture, tex_size: None, screen, mirror, quad_dirty: true })
        }
    }

    pub fn resize(&mut self, screen: (u32, u32)) {
        if screen != self.screen {
            self.screen = screen;
            self.quad_dirty = true;
        }
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

    pub fn draw(&mut self) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, self.screen.0 as i32, self.screen.1 as i32);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let Some(tex_size) = self.tex_size else { return };
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            if self.quad_dirty {
                let rect = letterbox(tex_size, self.screen);
                let verts = quad_vertices(rect, self.screen, self.mirror);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&verts));
                self.quad_dirty = false;
            }
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_texture(self.texture);
            self.gl.delete_buffer(self.vbo);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_program(self.program);
        }
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
