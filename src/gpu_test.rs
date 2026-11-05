//! The GPU test that `launch` runs before each start: a fixed OpenGL 3.3 workload in a
//! headless EGL context, timed per frame with GL_TIME_ELAPSED. The limits in graphics.rs
//! hold only for this exact workload (docs/graphics.md), so a change to a shader, a size or
//! a count needs a new calibration.

use std::ffi::{c_char, c_void};
use std::ptr;

use glow::HasContext;
use glutin_egl_sys::egl;
use libloading::Library;

use crate::graphics::GpuTest;

/// From EGL_MESA_platform_surfaceless, which glutin_egl_sys has no binding for.
const PLATFORM_SURFACELESS_MESA: egl::types::EGLenum = 0x31DD;

const WIDTH: i32 = 1920;
const HEIGHT: i32 = 1080;
const SHADOW: i32 = 2048;
const GRID: u32 = 400;
const INSTANCES: i32 = 4;
const WARM_UP: usize = 20;
const MEASURED: usize = 40;

const MESH_VS: &str = "#version 330 core\n\
    layout(location=0) in vec2 g; uniform mat4 mvp; uniform float inst; out vec3 n; out vec2 uv;\n\
    void main(){ vec2 q=g*2.0-1.0; float h=0.1*sin(q.x*40.0+inst)*cos(q.y*40.0);\n \
    n=normalize(vec3(-cos(q.x*40.0+inst), sin(q.y*40.0), 1.0)); uv=g;\n \
    gl_Position=mvp*vec4(q.x+inst*0.05, h, q.y, 1.0); }\n";
const MESH_FS: &str = "#version 330 core\n\
    in vec3 n; in vec2 uv; out vec4 c; uniform sampler2D shadow;\n\
    void main(){ float s=0.0; for(int i=0;i<16;i++){ s+=texture(shadow, uv+vec2(i%4,i/4)*0.001).r; }\n \
    c=vec4(max(dot(n,normalize(vec3(0.3,0.8,0.5))),0.0)*s/16.0*vec3(0.8,0.7,0.6),1.0); }\n";
const DEPTH_FS: &str = "#version 330 core\nvoid main(){}\n";
const QUAD_VS: &str = "#version 330 core\nout vec2 uv; void main(){ vec2 p=vec2((gl_VertexID<<1)&2, gl_VertexID&2);\n \
    uv=p; gl_Position=vec4(p*2.0-1.0,0.0,1.0); }\n";
const POST_FS: &str = "#version 330 core\nin vec2 uv; out vec4 c; uniform sampler2D src; uniform sampler2D depth;\n\
    void main(){ float d=texture(depth,uv).r; float ao=0.0; vec3 acc=vec3(0.0);\n \
    for(int i=0;i<24;i++){ float a=float(i)*2.39996; vec2 o=vec2(cos(a),sin(a))*(0.002+0.0004*float(i));\n  \
    float s=texture(depth,uv+o).r; ao+=step(s,d-0.001); acc+=texture(src,uv+o*0.5).rgb; }\n \
    c=vec4(acc/24.0*(1.0-ao/48.0),1.0); }\n";

const MVP: [f32; 16] = [0.9, 0.0, 0.0, 0.0, 0.0, 0.9, 0.3, 0.0, 0.0, -0.4, 0.5, 0.5, 0.0, 0.0, 0.5, 1.0];

type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;

struct Egl {
    api: egl::Egl,
    /// Own type, because the binding returns a function pointer that cannot be null.
    get_proc_address: GetProcAddress,
}

/// Tries the surfaceless platform first: on Mesa it picks the GPU that DRI_PRIME names, like
/// the game gets. Then the EGL devices, for drivers without it (NVIDIA). A software renderer
/// does not count: the game would not run on it, and the workload takes seconds there.
pub(crate) fn run() -> Result<GpuTest, String> {
    let egl = load()?;
    let mut failures = Vec::new();
    match unsafe { test_on(egl, PLATFORM_SURFACELESS_MESA, ptr::null_mut()) } {
        Ok(test) => return Ok(test),
        Err(e) => failures.push(format!("surfaceless: {e}")),
    }
    match unsafe { devices(egl) } {
        Ok(devices) => {
            for (i, device) in devices.into_iter().enumerate() {
                match unsafe { test_on(egl, egl::PLATFORM_DEVICE_EXT, device.cast_mut()) } {
                    Ok(test) => return Ok(test),
                    Err(e) => failures.push(format!("device {i}: {e}")),
                }
            }
        }
        Err(e) => failures.push(e),
    }
    Err(failures.join("; "))
}

/// At run time, so the binary starts on a system without libEGL. Never unloaded: a driver can
/// keep threads that run its code after eglTerminate.
fn load() -> Result<&'static Egl, String> {
    let library = unsafe { Library::new("libEGL.so.1") }.map_err(|e| e.to_string())?;
    let library: &'static Library = Box::leak(Box::new(library));
    let symbol = unsafe { library.get::<GetProcAddress>(b"eglGetProcAddress\0") };
    let get_proc_address = *symbol.map_err(|e| e.to_string())?;
    let api = egl::Egl::load_with(|name| {
        let name = format!("{name}\0");
        match unsafe { library.get::<*const c_void>(name.as_bytes()) } {
            Ok(symbol) => *symbol,
            Err(_) => unsafe { get_proc_address(name.as_ptr().cast()) },
        }
    });
    Ok(Box::leak(Box::new(Egl { api, get_proc_address })))
}

unsafe fn devices(egl: &Egl) -> Result<Vec<egl::types::EGLDeviceEXT>, String> {
    let api = &egl.api;
    if !api.QueryDevicesEXT.is_loaded() || !api.GetPlatformDisplay.is_loaded() {
        return Err("no EGL device enumeration".into());
    }
    let mut count = 0;
    if unsafe { api.QueryDevicesEXT(0, ptr::null_mut(), &mut count) } == egl::FALSE {
        return Err(egl_error(egl, "eglQueryDevicesEXT"));
    }
    let mut devices = vec![ptr::null(); count.max(0) as usize];
    if unsafe { api.QueryDevicesEXT(count, devices.as_mut_ptr(), &mut count) } == egl::FALSE {
        return Err(egl_error(egl, "eglQueryDevicesEXT"));
    }
    devices.truncate(count.max(0) as usize);
    Ok(devices)
}

fn egl_error(egl: &Egl, call: &str) -> String {
    format!("{call} failed (EGL error {:#x})", unsafe { egl.api.GetError() })
}

struct Display<'a> {
    egl: &'a Egl,
    raw: egl::types::EGLDisplay,
}

impl Drop for Display<'_> {
    fn drop(&mut self) {
        unsafe { self.egl.api.Terminate(self.raw) };
    }
}

/// Released and destroyed on drop, so no context is left when `launch` execs Wine.
struct Context<'a> {
    display: &'a Display<'a>,
    raw: egl::types::EGLContext,
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        let (api, display) = (&self.display.egl.api, self.display.raw);
        unsafe {
            api.MakeCurrent(display, egl::NO_SURFACE, egl::NO_SURFACE, egl::NO_CONTEXT);
            api.DestroyContext(display, self.raw);
        }
    }
}

unsafe fn test_on(egl: &Egl, platform: egl::types::EGLenum, native: *mut c_void) -> Result<GpuTest, String> {
    let api = &egl.api;
    if !api.GetPlatformDisplay.is_loaded() {
        return Err("no eglGetPlatformDisplay".into());
    }
    let raw = unsafe { api.GetPlatformDisplay(platform, native, ptr::null()) };
    if raw == egl::NO_DISPLAY {
        return Err(egl_error(egl, "eglGetPlatformDisplay"));
    }
    if unsafe { api.Initialize(raw, ptr::null_mut(), ptr::null_mut()) } == egl::FALSE {
        return Err(egl_error(egl, "eglInitialize"));
    }
    let display = Display { egl, raw };
    if unsafe { api.BindAPI(egl::OPENGL_API) } == egl::FALSE {
        return Err(egl_error(egl, "eglBindAPI"));
    }
    let config_attributes =
        [egl::RENDERABLE_TYPE, egl::OPENGL_BIT, egl::SURFACE_TYPE, 0, egl::NONE].map(|v| v as egl::types::EGLint);
    let mut config = ptr::null();
    let mut configs = 0;
    let chosen = unsafe { api.ChooseConfig(raw, config_attributes.as_ptr(), &mut config, 1, &mut configs) };
    if chosen == egl::FALSE || configs == 0 {
        config = ptr::null();
    }
    let context_attributes = [
        egl::CONTEXT_MAJOR_VERSION,
        3,
        egl::CONTEXT_MINOR_VERSION,
        3,
        egl::CONTEXT_OPENGL_PROFILE_MASK,
        egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
        egl::NONE,
    ]
    .map(|v| v as egl::types::EGLint);
    let context = unsafe { api.CreateContext(raw, config, egl::NO_CONTEXT, context_attributes.as_ptr()) };
    if context == egl::NO_CONTEXT {
        return Err(egl_error(egl, "eglCreateContext for OpenGL 3.3 core"));
    }
    let context = Context { display: &display, raw: context };
    if unsafe { api.MakeCurrent(raw, egl::NO_SURFACE, egl::NO_SURFACE, context.raw) } == egl::FALSE {
        return Err(egl_error(egl, "eglMakeCurrent"));
    }
    let gl = unsafe { glow::Context::from_loader_function_cstr(|name| (egl.get_proc_address)(name.as_ptr())) };
    let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
    if ["llvmpipe", "softpipe", "swrast"].iter().any(|software| renderer.contains(software)) {
        return Err(format!("{renderer} is a software renderer"));
    }
    let mut frames = unsafe { draw(&gl) }?;
    frames.sort_by(f64::total_cmp);
    let frame_ms = frames[frames.len() / 2];
    if frame_ms.is_nan() || frame_ms <= 0.0 {
        return Err(format!("the timer query on {renderer} measured no time"));
    }
    Ok(GpuTest { renderer, frame_ms })
}

/// The frame times in ms of the measured frames. Each frame: a depth pass of the mesh into
/// the shadow map, a lit pass that samples the shadow map, then 4 full-screen post passes.
unsafe fn draw(gl: &glow::Context) -> Result<Vec<f64>, String> {
    let textures = [
        (glow::DEPTH_COMPONENT24, SHADOW, SHADOW, glow::DEPTH_COMPONENT, glow::FLOAT),
        (glow::RGBA8, WIDTH, HEIGHT, glow::RGBA, glow::UNSIGNED_BYTE),
        (glow::DEPTH_COMPONENT24, WIDTH, HEIGHT, glow::DEPTH_COMPONENT, glow::FLOAT),
        (glow::RGBA8, WIDTH, HEIGHT, glow::RGBA, glow::UNSIGNED_BYTE),
    ];
    let mut tex = Vec::new();
    for (internal, width, height, format, kind) in textures {
        let texture = unsafe { gl.create_texture() }?;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            let pixels = glow::PixelUnpackData::Slice(None);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, internal as i32, width, height, 0, format, kind, pixels);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        }
        tex.push(texture);
    }
    let attachments: [&[(u32, usize)]; 3] = [
        &[(glow::DEPTH_ATTACHMENT, 0)],
        &[(glow::COLOR_ATTACHMENT0, 1), (glow::DEPTH_ATTACHMENT, 2)],
        &[(glow::COLOR_ATTACHMENT0, 3)],
    ];
    let mut fbo = Vec::new();
    for attached in attachments {
        let framebuffer = unsafe { gl.create_framebuffer() }?;
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer)) };
        for &(attachment, texture) in attached {
            unsafe { gl.framebuffer_texture_2d(glow::FRAMEBUFFER, attachment, glow::TEXTURE_2D, Some(tex[texture]), 0) };
        }
        fbo.push(framebuffer);
    }

    let vertices: Vec<u8> = (0..=GRID)
        .flat_map(|y| (0..=GRID).map(move |x| [x as f32 / GRID as f32, y as f32 / GRID as f32]))
        .flatten()
        .flat_map(f32::to_ne_bytes)
        .collect();
    let indices: Vec<u8> = (0..GRID)
        .flat_map(|y| (0..GRID).map(move |x| (y, x)))
        .flat_map(|(y, x)| {
            let a = y * (GRID + 1) + x;
            let (b, c) = (a + 1, a + GRID + 1);
            [a, c, b, b, c, c + 1]
        })
        .flat_map(u32::to_ne_bytes)
        .collect();
    let index_count = (GRID * GRID * 6) as i32;
    unsafe {
        let vao = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(gl.create_buffer()?));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertices, glow::STATIC_DRAW);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(gl.create_buffer()?));
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, &indices, glow::STATIC_DRAW);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 0, 0);
    }

    let mesh = unsafe { program(gl, MESH_VS, MESH_FS) }?;
    let depth = unsafe { program(gl, MESH_VS, DEPTH_FS) }?;
    let post = unsafe { program(gl, QUAD_VS, POST_FS) }?;
    let queries = (0..WARM_UP + MEASURED).map(|_| unsafe { gl.create_query() }).collect::<Result<Vec<_>, _>>()?;
    unsafe {
        gl.enable(glow::DEPTH_TEST);
        for (frame, query) in queries.iter().enumerate() {
            let shift = frame as f32 * 0.01;
            gl.begin_query(glow::TIME_ELAPSED, *query);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo[0]));
            gl.viewport(0, 0, SHADOW, SHADOW);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(depth));
            gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(depth, "mvp").as_ref(), false, &MVP);
            for i in 0..INSTANCES {
                gl.uniform_1_f32(gl.get_uniform_location(depth, "inst").as_ref(), i as f32 + shift);
                gl.draw_elements(glow::TRIANGLES, index_count, glow::UNSIGNED_INT, 0);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo[1]));
            gl.viewport(0, 0, WIDTH, HEIGHT);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(mesh));
            gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(mesh, "mvp").as_ref(), false, &MVP);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex[0]));
            for i in 0..INSTANCES {
                gl.uniform_1_f32(gl.get_uniform_location(mesh, "inst").as_ref(), i as f32 + shift);
                gl.draw_elements(glow::TRIANGLES, index_count, glow::UNSIGNED_INT, 0);
            }
            gl.disable(glow::DEPTH_TEST);
            gl.use_program(Some(post));
            gl.uniform_1_i32(gl.get_uniform_location(post, "src").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(post, "depth").as_ref(), 1);
            for pass in 0..4 {
                let odd = pass % 2 == 1;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo[if odd { 1 } else { 2 }]));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex[if odd { 3 } else { 1 }]));
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex[2]));
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                gl.active_texture(glow::TEXTURE0);
            }
            gl.enable(glow::DEPTH_TEST);
            gl.end_query(glow::TIME_ELAPSED);
        }
        gl.finish();
        let error = gl.get_error();
        if error != glow::NO_ERROR {
            return Err(format!("OpenGL error {error:#x}"));
        }
        let frames = queries[WARM_UP..].iter().map(|query| gl.get_query_parameter_u64(*query, glow::QUERY_RESULT));
        Ok(frames.map(|ns| ns as f64 / 1e6).collect())
    }
}

unsafe fn program(gl: &glow::Context, vertex: &str, fragment: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        for (kind, source) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(format!("shader: {}", gl.get_shader_info_log(shader).trim()));
            }
            gl.attach_shader(program, shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(format!("program: {}", gl.get_program_info_log(program).trim()));
        }
        Ok(program)
    }
}

#[cfg(test)]
mod tests {
    /// Needs a GPU: `cargo test -- --ignored --nocapture gpu_test`.
    #[test]
    #[ignore]
    fn the_gpu_test_runs_on_this_machine() {
        let test = super::run().unwrap();
        eprintln!("{} ms on {}", test.frame_ms, test.renderer);
        assert!(test.frame_ms > 0.0);
    }
}
